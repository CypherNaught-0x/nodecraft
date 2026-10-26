//! Bounded view-only inspection of local text actions. Values are read, never recomputed.
use crate::theme::Tokens;
use egui::{Rect, RichText};
use nodecraft_engine::Session;
use nodecraft_graph::Canvas;
use nodecraft_nodes::{Category, JobSpec, PlanCtx, Registry};
use nodecraft_types::{Blake3, CanvasId, NodeId, Value};
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

const MAX_ROWS: usize = 16_384;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const FULL_BYTES: usize = 8 * 1024 * 1024;
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

fn value<'a>(canvas: &'a Canvas, kinds: &Registry, reference: &Reference) -> Option<Cow<'a, Value>> {
    let node = canvas.nodes().get(&reference.node)?;
    let index = node.outputs.iter().position(|port| port.name == reference.port)?;
    // The common source is borrowed, including megabyte texts shared by many actions.
    if node.kind == "source.text" && reference.port == "out" {
        return Some(node.params.get("text").map(Cow::Borrowed).unwrap_or_else(|| Cow::Owned(Value::Text(String::new()))));
    }
    if let Ok(kind) = kinds.get(&node.kind)
        && kind.descriptor().category == Category::Source
    {
        return kind.plan(&PlanCtx { node, inputs: &[], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).ok()?.into_iter().find_map(|job| match job {
            JobSpec::Source { output, value, .. } if output == reference.port => Some(Cow::Owned(value)),
            _ => None,
        });
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
    while let Some(value) = stack.pop() {
        visits = visits.saturating_add(1);
        if visits > MAX_ROWS {
            return "List exceeds the full preview limit.".into();
        }
        match value {
            Value::List(values) if stack.len().saturating_add(values.len()) <= MAX_ROWS => stack.extend(values.iter().rev()),
            Value::Text(text) if out.len().saturating_add(text.len()).saturating_add(1) <= FULL_BYTES => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
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
                let output = value(canvas, &kinds, &reference);
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
                                    value(canvas, &kinds, &reference).map_or_else(|| "No completed value yet".into(), |v| excerpt(&v, t.preview_chars, 3))
                                })
                                .clone();
                            let source = canvas.nodes().get(from).map_or("missing source", |n| if n.title.is_empty() { &n.kind } else { &n.title });
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
            full.text = value(canvas, &kinds, &full.reference).map_or_else(|| "This value is no longer available.".into(), |v| full_text(&v));
        }
    }

    pub fn rows(&self, node: NodeId) -> Option<&[Row]> {
        self.entries.get(&node).map(Vec::as_slice)
    }

    pub fn open(&mut self, session: &Session, node: NodeId, row: usize) {
        let Some(row) = self.entries.get(&node).and_then(|rows| rows.get(row)) else { return };
        let Some(reference) = row.reference.clone() else { return };
        let Some(canvas) = self.canvas.and_then(|id| session.document().canvases().get(&id)) else { return };
        let text = value(canvas, session.kinds(), &reference).map_or_else(|| "No completed value yet".into(), |v| full_text(&v));
        self.full = Some(Full { reference, title: row.label.clone(), text });
    }

    pub fn overlay(&mut self, ctx: &egui::Context, t: &Tokens) -> bool {
        let Some(full) = &self.full else { return false };
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("local_text_preview_full")).show(ctx, |ui| {
            ui.set_width(t.viewer_width.min(ctx.content_rect().width() - t.card_padding * 2.0).max(t.card_width));
            ui.heading(&full.title);
            egui::ScrollArea::vertical().max_height(t.comments_height).show(ui, |ui| {
                ui.add(egui::Label::new(if full.text.is_empty() { "(empty)" } else { &full.text }).selectable(true).wrap());
            });
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
            egui::ScrollArea::vertical().id_salt("local_rows").max_height(ui.available_height()).auto_shrink([false, false]).show_rows(
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
        let read = value(canvas, s.kinds(), &reference).unwrap();
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
        let huge = Value::List(vec![Value::List(vec![]); MAX_ROWS + 1]);
        assert!(excerpt(&huge, 256, 3).contains("limit"));
    }
}
