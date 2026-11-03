//! Bounded view-only inspection of local text actions. Values are read, never recomputed.
use crate::theme::Tokens;
use egui::{Rect, RichText};
use nodecraft_engine::Session;
use nodecraft_graph::Canvas;
use nodecraft_nodes::{Category, JobSpec, PlanCtx, Registry};
use nodecraft_types::{Blake3, CanvasId, NodeId, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const MAX_ROWS: usize = 16_384;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const FULL_BYTES: usize = 8 * 1024 * 1024;
// Bound per-frame label copying, hashing and text layout independently of retained/copyable text.
const DISPLAY_BYTES: usize = 64 * 1024;
pub(crate) type Failures = BTreeMap<(CanvasId, NodeId), (Blake3, String)>;

pub(crate) fn eligible(descriptor: Option<&nodecraft_nodes::NodeDescriptor>) -> bool {
    descriptor.is_some_and(|d| {
        d.category == Category::Action && !d.outputs.is_empty() && d.outputs.iter().all(|p| p.port_type.modality == nodecraft_types::Modality::Text)
    })
}

#[derive(Clone)]
struct Reference {
    node: NodeId,
    port: String,
}

pub(crate) struct Row {
    pub label: String,
    pub text: String,
    pub error: bool,
    reference: Option<Reference>,
}

struct Full {
    reference: Reference,
    title: String,
    text: String,
    display_end: usize,
}

impl Full {
    fn new(reference: Reference, title: String, text: String) -> Self {
        let display_end = text.floor_char_boundary(DISPLAY_BYTES.min(text.len()));
        Self { reference, title, text, display_end }
    }
    fn update(&mut self, text: String) {
        self.display_end = text.floor_char_boundary(DISPLAY_BYTES.min(text.len()));
        self.text = text;
    }
    fn displayed(&self) -> &str {
        self.text.get(..self.display_end).unwrap_or_default()
    }
}

#[derive(Default)]
pub(crate) struct Cache {
    identity: Option<Arc<crate::navigation::SessionIdentity>>,
    kinds: Option<Arc<Registry>>,
    revision: u64,
    canvas: Option<CanvasId>,
    entries: BTreeMap<NodeId, Vec<Row>>,
    full: Option<Full>,
    #[cfg(test)]
    pub rebuilds: usize,
}

struct ReadBudget {
    values: usize,
    bytes: usize,
}

fn charge(value: &Value, budget: &mut ReadBudget) -> Option<()> {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        budget.values = budget.values.checked_sub(1)?;
        budget.bytes = budget.bytes.checked_sub(std::mem::size_of::<Value>())?;
        match value {
            Value::Text(text) => budget.bytes = budget.bytes.checked_sub(text.len())?,
            Value::List(values) => {
                if stack.len().saturating_add(values.len()) > budget.values {
                    return None;
                }
                stack.extend(values);
            }
            _ => {}
        }
    }
    Some(())
}

fn value<'a>(canvas: &'a Canvas, kinds: &Registry, reference: &Reference, routing: Option<&nodecraft_exec::routing::Incoming<'_>>) -> Option<Cow<'a, Value>> {
    read_value(canvas, kinds, reference, routing, &mut BTreeSet::new(), &mut ReadBudget { values: MAX_ROWS * 4, bytes: FULL_BYTES })
}

fn read_value<'a>(
    canvas: &'a Canvas,
    kinds: &Registry,
    reference: &Reference,
    routing: Option<&nodecraft_exec::routing::Incoming<'_>>,
    seen: &mut BTreeSet<NodeId>,
    budget: &mut ReadBudget,
) -> Option<Cow<'a, Value>> {
    budget.values = budget.values.checked_sub(1)?;
    let node = canvas.nodes().get(&reference.node)?;
    let index = node.outputs.iter().position(|port| port.name == reference.port)?;
    if node.kind == "source.text" && reference.port == "out" {
        return Some(node.params.get("text").map(Cow::Borrowed).unwrap_or_else(|| Cow::Owned(Value::Text(String::new()))));
    }
    if node.kind == nodecraft_nodes::batch::ID {
        if seen.len() >= 32 || !seen.insert(node.id) {
            return None;
        }
        kinds.get(&node.kind).ok()?.validate_instance(node, true).ok()?;
        let routing = routing?;
        if routing.invalid.keys().any(|(target, _)| *target == node.id) {
            return None;
        }
        let mut wired = Vec::new();
        for (_, _, from, port) in routing.feeds.get(&node.id).into_iter().flatten() {
            let source = Reference { node: *from, port: (*port).into() };
            let read = read_value(canvas, kinds, &source, Some(routing), seen, budget)?;
            charge(&read, budget)?;
            let upstream = canvas.nodes().get(from)?;
            let list = upstream.outputs.iter().find(|p| p.name == *port).is_some_and(|p| p.port_type.many);
            let fanned = upstream.history.iter().find(|g| Some(g.id) == upstream.active).is_some_and(|g| !g.items.is_empty());
            match read.into_owned() {
                Value::List(values) if list && fanned => {
                    for value in values {
                        match value {
                            Value::List(values) => wired.extend(values),
                            value => wired.push(value),
                        }
                    }
                }
                Value::List(values) if list || fanned || upstream.kind == nodecraft_nodes::batch::ID => wired.extend(values),
                value => wired.push(value),
            }
            if wired.len() > nodecraft_nodes::batch::MAX_ITEMS {
                return None;
            }
        }
        for parameter in node.params.values() {
            charge(parameter, budget)?;
        }
        let items = nodecraft_nodes::batch::items(node, wired).ok()?;
        seen.remove(&node.id);
        return Some(Cow::Owned(Value::List(items)));
    }
    if let Ok(kind) = kinds.get(&node.kind)
        && kind.descriptor().category == Category::Source
    {
        for parameter in node.params.values() {
            charge(parameter, budget)?;
        }
        if nodecraft_nodes::folder::loops_over(node, &reference.port) {
            return nodecraft_nodes::folder::items(node, &reference.port).ok().map(|items| Cow::Owned(Value::List(items)));
        }
        let jobs = kind.plan(&PlanCtx { node, inputs: &[], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).ok()?;
        let mut values = Vec::new();
        for job in jobs {
            if let JobSpec::Source { output, value, .. } = job
                && output == reference.port
            {
                charge(&value, budget)?;
                values.push(value);
            }
        }
        return if node.outputs.get(index)?.port_type.many { Some(Cow::Owned(Value::List(values))) } else { values.into_iter().next().map(Cow::Owned) };
    }
    let generation = node.history.iter().find(|g| Some(g.id) == node.active && !g.is_partial() && g.outputs.len() == node.outputs.len())?;
    generation.outputs.get(index).map(Cow::Borrowed)
}

/// A short text/list excerpt, with independently bounded traversal of hostile nested lists.
fn excerpt(value: &Value, chars: usize, lines: usize) -> String {
    let mut out = String::new();
    let mut left = chars;
    let mut line = 1usize;
    let mut stack = vec![value];
    let mut visits = 0usize;
    while let Some(value) = stack.pop() {
        visits = visits.saturating_add(1);
        if visits > MAX_ROWS || left == 0 {
            out.push('…');
            break;
        }
        if let Value::List(values) = value {
            if values.is_empty() {
                // Empty nested lists contribute no text; the root's empty state is below.
            } else if stack.len().saturating_add(values.len()) <= MAX_ROWS {
                stack.extend(values.iter().rev());
            } else {
                out.push_str("… (list preview limit)");
                break;
            }
            continue;
        }
        let text = match value {
            Value::Text(text) if !text.is_empty() => text.as_str(),
            Value::Text(_) => "(empty)",
            _ => "(not text)",
        };
        if !out.is_empty() {
            if line >= lines {
                out.push('…');
                break;
            }
            out.push('\n');
            left = left.saturating_sub(1);
            line = line.saturating_add(1);
        }
        for ch in text.chars() {
            if left == 0 || ch == '\n' && line >= lines {
                out.push('…');
                return out;
            }
            left = left.saturating_sub(1);
            if ch == '\n' {
                line = line.saturating_add(1);
            }
            out.push(ch);
        }
    }
    if out.is_empty() { "(empty list)".into() } else { out }
}

fn full_text(value: &Value) -> String {
    if let Value::Text(text) = value {
        return if text.len() <= FULL_BYTES { text.clone() } else { "Text exceeds the full preview limit.".into() };
    }
    let mut out = String::new();
    let mut stack = vec![value];
    let mut visits = 0usize;
    let mut emitted = false;
    while let Some(value) = stack.pop() {
        visits = visits.saturating_add(1);
        if visits > MAX_ROWS {
            return "List exceeds the full preview limit.".into();
        }
        match value {
            Value::List(values) if stack.len().saturating_add(values.len()) <= MAX_ROWS => stack.extend(values.iter().rev()),
            Value::Text(text) if out.len().saturating_add(text.len()).saturating_add(usize::from(emitted)) <= FULL_BYTES => {
                if emitted {
                    out.push('\n');
                }
                out.push_str(text);
                emitted = true;
            }
            _ => return "Value exceeds the full text preview limits.".into(),
        }
    }
    out
}

fn admit(rows: &mut Vec<Row>, bytes: &mut usize, total: &mut usize, row: Row) -> bool {
    let cost =
        row.label.capacity().saturating_add(row.text.capacity()).saturating_add(row.reference.as_ref().map_or(0, |r| r.port.capacity())).saturating_add(512);
    let next = bytes.saturating_add(cost);
    if *total >= MAX_ROWS || next > MAX_BYTES {
        return false;
    }
    *bytes = next;
    *total = total.saturating_add(1);
    rows.push(row);
    true
}

impl Cache {
    pub fn invalidate(&mut self) {
        self.kinds = None;
    }
    pub fn prepare(&mut self, session: &mut Session, canvas_id: CanvasId, failures: &Failures, t: &Tokens) {
        let identity = crate::navigation::Geometry::identity(session);
        let kinds = session.kinds_shared();
        let replaced = self.identity.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &identity)) || self.canvas != Some(canvas_id);
        if !replaced && self.revision == session.revision() && self.kinds.as_ref().is_some_and(|old| Arc::ptr_eq(old, &kinds)) {
            return;
        }
        if replaced {
            self.full = None;
        }
        self.identity = Some(identity);
        self.canvas = Some(canvas_id);
        self.revision = session.revision();
        self.kinds = Some(Arc::clone(&kinds));
        self.entries.clear();
        #[cfg(test)]
        {
            self.rebuilds = self.rebuilds.saturating_add(1);
        }
        let Some(canvas) = session.document().canvases().get(&canvas_id) else { return };
        if !canvas.nodes().values().any(|n| kinds.get(&n.kind).is_ok_and(|k| eligible(Some(k.descriptor())))) {
            self.full = None;
            return;
        }
        let routing = nodecraft_exec::routing::incoming(canvas, &kinds);
        let mut hasher = nodecraft_exec::staleness::InputHasher::new(canvas, &kinds, nodecraft_exec::staleness::MAX_QUERY_BYTES).ok();
        let mut used = 0usize;
        let mut total = 0usize;
        let mut memo = BTreeMap::<(NodeId, String), String>::new();
        for node in canvas.nodes().values().filter(|n| kinds.get(&n.kind).is_ok_and(|k| eligible(Some(k.descriptor())))) {
            if total >= MAX_ROWS || used >= MAX_BYTES {
                break;
            }
            let mut rows = Vec::new();
            let mut limited = false;
            let hash = hasher.as_mut().and_then(|h| h.hash(canvas, &kinds, node.id, &BTreeMap::new()).ok()).flatten();
            if let Some((_, reason)) = failures.get(&(canvas_id, node.id)).filter(|(old, _)| Some(*old) == hash) {
                limited = !admit(
                    &mut rows,
                    &mut used,
                    &mut total,
                    Row {
                        label: format!("Error ({}):", node.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")),
                        text: reason.clone(),
                        error: true,
                        reference: None,
                    },
                );
            }
            for port in &node.outputs {
                if limited {
                    break;
                }
                let reference = Reference { node: node.id, port: port.name.clone() };
                let output = value(canvas, &kinds, &reference, routing.as_ref().ok());
                let stale = node.history.iter().find(|g| Some(g.id) == node.active).is_some_and(|g| g.input_hash != hash);
                limited = !admit(
                    &mut rows,
                    &mut used,
                    &mut total,
                    Row {
                        label: format!("Output {}{}", port.name, if stale { " (updating)" } else { "" }),
                        text: output.as_ref().map_or_else(|| "No output yet".into(), |v| excerpt(v, t.preview_chars, 3)),
                        error: false,
                        reference: output.is_some().then_some(reference),
                    },
                );
            }
            match &routing {
                Ok(routing) => {
                    let feeds = routing.feeds.get(&node.id).map(Vec::as_slice).unwrap_or_default();
                    for port in &node.inputs {
                        if limited {
                            break;
                        }
                        let mut any = false;
                        for (_, order, from, output) in feeds.iter().filter(|feed| feed.0 == port.name) {
                            any = true;
                            let reference = Reference { node: *from, port: (*output).into() };
                            let summary = memo
                                .entry((*from, (*output).to_owned()))
                                .or_insert_with(|| {
                                    value(canvas, &kinds, &reference, Some(routing))
                                        .map_or_else(|| "Value unavailable: no completed result or preview limit".into(), |v| excerpt(&v, t.preview_chars, 3))
                                })
                                .clone();
                            let source = canvas.nodes().get(from).map_or("Missing source", |n| {
                                if n.title.is_empty() { kinds.get(&n.kind).map_or("Source", |kind| kind.descriptor().label.as_str()) } else { &n.title }
                            });
                            let source: String = source.chars().take(64).collect();
                            let label = format!("Input {} · {} · {source}", port.name, order.saturating_add(1));
                            limited = !admit(&mut rows, &mut used, &mut total, Row { label, text: summary, error: false, reference: Some(reference) });
                            if limited {
                                break;
                            }
                        }
                        if !any {
                            limited = !admit(
                                &mut rows,
                                &mut used,
                                &mut total,
                                Row { label: format!("Input {}", port.name), text: "Connect an input first".into(), error: true, reference: None },
                            );
                        }
                    }
                    if !limited && routing.invalid.keys().any(|(id, _)| *id == node.id) {
                        limited = !admit(
                            &mut rows,
                            &mut used,
                            &mut total,
                            Row { label: "Input error".into(), text: "An upstream router is invalid.".into(), error: true, reference: None },
                        );
                    }
                }
                Err(error) if !limited => {
                    limited = !admit(
                        &mut rows,
                        &mut used,
                        &mut total,
                        Row { label: "Input error".into(), text: error.message.clone(), error: true, reference: None },
                    );
                }
                _ => {}
            }
            // One fixed-size footer is reserved outside the shared row budget; then stop.
            if limited {
                rows.push(Row {
                    label: "Preview limit".into(),
                    text: "More values omitted; reduce the number of connections.".into(),
                    error: false,
                    reference: None,
                });
            }
            self.entries.insert(node.id, rows);
            if limited {
                break;
            }
        }
        if let Some(full) = &mut self.full {
            full.update(
                value(canvas, &kinds, &full.reference, routing.as_ref().ok()).map_or_else(|| "This value is no longer available.".into(), |v| full_text(&v)),
            );
        }
    }

    pub fn rows(&self, node: NodeId) -> Option<&[Row]> {
        self.entries.get(&node).map(Vec::as_slice)
    }

    pub fn open(&mut self, session: &Session, node: NodeId, row: usize) {
        let Some(row) = self.entries.get(&node).and_then(|rows| rows.get(row)) else { return };
        let Some(reference) = row.reference.clone() else { return };
        let Some(canvas) = self.canvas.and_then(|id| session.document().canvases().get(&id)) else { return };
        let routing = nodecraft_exec::routing::incoming(canvas, session.kinds()).ok();
        let text = value(canvas, session.kinds(), &reference, routing.as_ref())
            .map_or_else(|| "Value unavailable: no completed result or preview limit".into(), |v| full_text(&v));
        self.full = Some(Full::new(reference, row.label.clone(), text));
    }

    pub fn overlay(&mut self, ctx: &egui::Context, t: &Tokens) -> bool {
        let Some(full) = &self.full else { return false };
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("local_text_preview_full")).show(ctx, |ui| {
            ui.set_width(t.viewer_width.min(ctx.content_rect().width() - t.card_padding * 2.0).max(t.card_width));
            ui.heading(&full.title);
            if full.display_end < full.text.len() {
                ui.label(
                    RichText::new(format!("Showing the first {} of {} bytes. Copy full text to access the rest.", full.display_end, full.text.len()))
                        .color(t.text_muted),
                );
            }
            egui::ScrollArea::vertical().max_height(t.comments_height).show(ui, |ui| {
                ui.add(egui::Label::new(if full.text.is_empty() { "(empty)" } else { full.displayed() }).selectable(true).wrap());
            });
            if ui.button("Copy full text").clicked() {
                ctx.copy_text(full.text.clone());
            }
            close = ui.button("Close text preview").clicked();
        });
        if close || modal.should_close() {
            self.full = None;
        }
        true
    }
}

pub(crate) fn show(ui: &mut egui::Ui, area: Rect, rows: Option<&[Row]>, t: &Tokens) -> Option<usize> {
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("local_text").max_rect(area));
    child.set_clip_rect(area.intersect(ui.clip_rect()));
    let mut open = None;
    egui::Frame::NONE.fill(t.prompt_fill).inner_margin(t.control_gap).corner_radius(t.card_radius / 2).show(&mut child, |ui| {
        if let Some(rows) = rows {
            let row_height = t.body_size * 4.0 + ui.spacing().interact_size.y + t.control_gap * 4.0;
            // Under the rows, so the card layer's hit test says when the pointer is over them.
            let wheel = ui.interact(ui.available_rect_before_wrap(), ui.id().with("local_rows_wheel"), egui::Sense::hover());
            let out = egui::ScrollArea::vertical().id_salt("local_rows").max_height(ui.available_height()).auto_shrink([false, false]).show_rows(
                ui,
                row_height,
                rows.len(),
                |ui, range| {
                    for index in range {
                        let Some(row) = rows.get(index) else { continue };
                        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_height), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.add(egui::Label::new(RichText::new(&row.label).color(if row.error { t.danger } else { t.text_muted })).truncate());
                            let mut job = egui::text::LayoutJob::simple(
                                row.text.clone(),
                                egui::FontId::proportional(t.body_size),
                                if row.error { t.danger } else { t.text },
                                ui.available_width(),
                            );
                            job.wrap.max_rows = 3;
                            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                            ui.add(egui::Label::new(galley).selectable(true));
                            if row.reference.is_some() && ui.add(egui::Button::new(format!("Expand {}", row.label)).truncate()).clicked() {
                                open = Some(index);
                            }
                        });
                    }
                },
            );
            // Long previews scroll first; the canvas pans by what is left (#413).
            crate::scroll_chain::take(ui, wheel.contains_pointer(), &out, [false, true]);
        } else {
            ui.label("Text preview unavailable; reduce the number of connections.");
        }
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_types::{EdgeId, ProjectId};
    use serde_json::json;

    const C: CanvasId = CanvasId::from_bytes([1; 16]);
    const S: NodeId = NodeId::from_bytes([2; 16]);
    const A: NodeId = NodeId::from_bytes([3; 16]);

    fn session() -> Session {
        let mut project = Project::new(ProjectId::from_bytes([4; 16]), "Preview").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let mut s = Session::new(project).unwrap();
        for (id, kind) in [(S, "source.text"), (A, "text.concat")] {
            s.dispatch("node.create", &json!({"canvas":C,"id":id,"kind":kind,"pos":{"x":0,"y":0}})).unwrap();
        }
        s.dispatch("node.param_set", &json!({"canvas":C,"id":S,"name":"text","value":Value::Text("first".into())})).unwrap();
        s.dispatch(
            "edge.connect",
            &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([5;16]),"from":{"node":S,"port":"out"},"to":{"node":A,"port":"texts"},"order":0}}),
        )
        .unwrap();
        s
    }

    #[test]
    fn cache_rebuilds_on_edits_and_undo_but_not_idle_frames_and_never_mutates_the_document() {
        let mut s = session();
        let before = s.document().clone();
        let revision = s.revision();
        let mut cache = Cache::default();
        for _ in 0..20 {
            cache.prepare(&mut s, C, &Failures::new(), &crate::theme::DARK);
        }
        assert_eq!(cache.rebuilds, 1);
        assert_eq!((s.document(), s.revision()), (&before, revision));
        assert!(cache.rows(A).unwrap().iter().any(|row| row.text == "first"));
        s.dispatch("node.param_set", &json!({"canvas":C,"id":S,"name":"text","value":Value::Text("second".into())})).unwrap();
        cache.prepare(&mut s, C, &Failures::new(), &crate::theme::DARK);
        assert_eq!(cache.rebuilds, 2);
        assert!(cache.rows(A).unwrap().iter().any(|row| row.text == "second"));
        s.dispatch("edit.undo", &json!({})).unwrap();
        cache.prepare(&mut s, C, &Failures::new(), &crate::theme::DARK);
        assert!(cache.rows(A).unwrap().iter().any(|row| row.text == "first"));
        assert_eq!(cache.rebuilds, 3);
    }

    #[test]
    fn source_text_is_borrowed_and_router_inputs_keep_the_same_order() {
        let mut s = session();
        let reference = Reference { node: S, port: "out".into() };
        let canvas = s.document().canvases().get(&C).unwrap();
        let read = value(canvas, s.kinds(), &reference, None).unwrap();
        assert!(matches!(read, Cow::Borrowed(_)));
        assert!(std::ptr::eq(read.as_ref(), canvas.nodes().get(&S).unwrap().params.get("text").unwrap()));
        let router = NodeId::from_bytes([7; 16]);
        s.dispatch("node.create", &json!({"canvas":C,"id":router,"kind":"structure.router","pos":{"x":0,"y":0}})).unwrap();
        s.dispatch("edge.disconnect", &json!({"canvas":C,"id":EdgeId::from_bytes([5;16])})).unwrap();
        for (id, from, to, port) in [(8, S, router, "in"), (9, router, A, "texts")] {
            s.dispatch(
                "edge.connect",
                &json!({"canvas":C,"edge":{"id":EdgeId::from_bytes([id;16]),"from":{"node":from,"port":"out"},"to":{"node":to,"port":port},"order":0}}),
            )
            .unwrap();
        }
        let mut cache = Cache::default();
        cache.prepare(&mut s, C, &Failures::new(), &crate::theme::DARK);
        let input = cache.rows(A).unwrap().iter().find(|row| row.label.starts_with("Input")).unwrap();
        assert_eq!(input.text, "first");
        assert_eq!(input.reference.as_ref().unwrap().node, S);
    }

    #[test]
    fn excerpts_bound_unicode_lines_and_nested_lists_and_full_text_keeps_the_tail() {
        let text = format!("{}\nsecond\nthird\nfourth", "🦊 café".repeat(1000));
        let value = Value::List(vec![Value::List(vec![]), Value::Text(text.clone())]);
        let short = excerpt(&value, 256, 3);
        assert!(short.chars().count() <= 257);
        assert!(short.ends_with('…'));
        assert_eq!(full_text(&Value::Text(text.clone())), text);
        assert_eq!(excerpt(&Value::Text("a\nb\nc\nd".into()), 256, 3), "a\nb\nc…");
        assert_eq!(excerpt(&Value::Text(String::new()), 256, 3), "(empty)");
        assert_eq!(excerpt(&Value::List(vec![]), 256, 3), "(empty list)");
        assert_eq!(full_text(&Value::List(vec![Value::Text(String::new()), Value::Text("beta".into())])), "\nbeta");
        assert_eq!(full_text(&Value::List(vec![Value::Text(String::new()), Value::Text(String::new())])), "\n");
        let huge = Value::List(vec![Value::List(vec![]); MAX_ROWS + 1]);
        assert!(excerpt(&huge, 256, 3).contains("limit"));
    }

    #[test]
    fn expanded_modal_bounds_megabytes_of_unicode_text_and_copies_the_retained_value() {
        use egui_kittest::{Harness, kittest::Queryable};
        let text = format!("{}TAIL", "🦊 café\n".repeat(300_000));
        assert!(text.len() > 3 * 1024 * 1024);
        let full = Full::new(Reference { node: S, port: "out".into() }, "Public large output".into(), text.clone());
        assert!(full.display_end <= DISPLAY_BYTES);
        assert!(text.is_char_boundary(full.display_end));
        let shown = full.displayed().to_owned();
        let cache = Cache { full: Some(full), ..Default::default() };
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 760.0)).build_ui_state(
            |ui, cache: &mut Cache| {
                cache.overlay(ui.ctx(), &crate::theme::DARK);
            },
            cache,
        );
        for _ in 0..3 {
            h.step();
            h.get_by_label(&shown);
            assert!(h.query_by_label(&text).is_none(), "the full retained value must never enter a label");
            assert!(h.query_by_label_contains("Copy full text to access the rest.").is_some());
        }
        if let Some(path) = std::env::var_os("NODECRAFT_TEXT969_EXPAND_CAPTURE") {
            h.render().unwrap().save(std::path::PathBuf::from(path)).unwrap();
        }
        h.get_by_label("Copy full text").click();
        h.step();
        assert!(h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(copied) if copied == &text)));
        assert_eq!(h.state().full.as_ref().unwrap().text, text);
        let full = h.state_mut().full.as_mut().unwrap();
        full.update("short\nvalue".into());
        assert_eq!(full.displayed(), "short\nvalue");
        assert_eq!(full.display_end, full.text.len());
    }

    #[test]
    fn folder_list_and_looping_names_preview_every_source_value() {
        let mut s = session();
        let id = NodeId::from_bytes([40; 16]);
        let config = nodecraft_nodes::folder::config(
            nodecraft_types::Modality::Text,
            "Public samples",
            vec![("alpha.txt".into(), Value::Text("a".into())), ("beta.txt".into(), Value::Text("b".into()))],
        )
        .unwrap();
        let node = s.kinds().get(nodecraft_nodes::folder::ID).unwrap().instantiate_instance(id, config, &Default::default()).unwrap();
        let expected = nodecraft_nodes::folder::values(&node, nodecraft_nodes::folder::ALL_NAMES).unwrap();
        assert_eq!(expected.len(), 2);
        s.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
        let canvas = s.document().canvases().get(&C).unwrap();
        for port in [nodecraft_nodes::folder::ALL_NAMES, nodecraft_nodes::folder::NAME] {
            let read = value(canvas, s.kinds(), &Reference { node: id, port: port.into() }, None).unwrap();
            assert_eq!(read.as_ref(), &Value::List(expected.clone()));
        }
    }
}
