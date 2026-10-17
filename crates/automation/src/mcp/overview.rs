//! Borrowed encoded admission for project overview pages, before response text is copied.
use super::{invalid, json_len};
use nodecraft_graph::Project;
use nodecraft_types::{CanvasId, Error, ProjectId};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct ProjectSummary<'a> {
    id: ProjectId,
    title: &'a str,
}
#[derive(Serialize)]
struct CanvasSummary<'a> {
    id: CanvasId,
    title: &'a str,
    nodes: usize,
    edges: usize,
}
#[derive(Serialize)]
struct Page<'a, 'b> {
    project: ProjectSummary<'a>,
    canvases: &'b [CanvasSummary<'a>],
    revision: u64,
    total: usize,
    next_offset: Option<usize>,
}
fn encoded_size(base: usize, entries: usize, next: Option<usize>) -> Result<usize, Error> {
    // The empty envelope already includes [] and next_offset:null (four bytes).
    Ok(base.saturating_sub(4).saturating_add(json_len(&next)?).saturating_add(entries))
}

pub(super) fn page(project: &Project, offset: usize, limit: usize, revision: u64, budget: usize) -> Result<Value, Error> {
    let total = project.canvases().len();
    let canvases = project.canvases();
    let project = ProjectSummary { id: project.id(), title: project.title() };
    let mut summaries = Vec::new();
    let base = json_len(&Page { project: ProjectSummary { ..project }, canvases: &[], revision, total, next_offset: None })?;
    if base > budget {
        return Err(invalid("project overview metadata exceeds the encoded page budget"));
    }
    let mut used = 0usize;
    let mut next_offset = None;
    // Only references and small counters are accumulated. No title is copied into an owned
    // response Value until every selected entry and the entire envelope have been admitted.
    for canvas in canvases.values().skip(offset).take(limit) {
        let summary = CanvasSummary { id: canvas.id(), title: canvas.title(), nodes: canvas.nodes().len(), edges: canvas.edges().len() };
        let cost = json_len(&summary)?.saturating_add(usize::from(!summaries.is_empty()));
        let next = offset.saturating_add(summaries.len()).saturating_add(1);
        let continuation = (next < total).then_some(next);
        let candidate = used.saturating_add(cost);
        if encoded_size(base, candidate, continuation)? > budget {
            if summaries.is_empty() {
                return Err(invalid("a canvas summary exceeds the encoded page budget"));
            }
            break;
        }
        used = candidate;
        summaries.push(summary);
        next_offset = continuation;
    }
    serde_json::to_value(Page { project, canvases: &summaries, revision, total, next_offset }).map_err(|_| super::internal())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change};

    #[test]
    fn borrowed_envelope_and_entry_have_exact_and_one_over_boundaries() {
        let mut project = Project::new(ProjectId::from_bytes([1; 16]), "\u{1}\"\\").unwrap();
        let empty = page(&project, 0, 1, u64::MAX, usize::MAX).unwrap();
        let empty_len = json_len(&empty).unwrap();
        assert_eq!(json_len(&page(&project, 0, 1, u64::MAX, empty_len).unwrap()).unwrap(), empty_len);
        assert!(page(&project, 0, 1, u64::MAX, empty_len - 1).unwrap_err().message.contains("metadata"));
        for n in 1..=2 {
            Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([n; 16]), "\u{1}\"\\") }.apply(&mut project).unwrap();
        }
        let one = page(&project, 0, 1, u64::MAX, usize::MAX).unwrap();
        assert_eq!(one["next_offset"], 1);
        let one_len = json_len(&one).unwrap();
        assert_eq!(page(&project, 0, 2, u64::MAX, one_len).unwrap(), one);
        assert!(page(&project, 0, 1, u64::MAX, one_len - 1).unwrap_err().message.contains("canvas summary"));
        let last = page(&project, 1, 1, u64::MAX, usize::MAX).unwrap();
        assert!(last["next_offset"].is_null());
        assert_eq!(json_len(&page(&project, 1, 1, u64::MAX, json_len(&last).unwrap()).unwrap()).unwrap(), json_len(&last).unwrap());
    }
}
