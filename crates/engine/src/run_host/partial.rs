//! Keeping a partial text answer (#504). A text job streams its answer into the job's view as it
//! arrives; only a finished answer is recorded. A job that was cancelled or failed keeps what it
//! streamed, and the person may keep it on the card: it joins the card's history without
//! replacing the answer it shows.
use super::RunHost;
use crate::Session;
use crate::run_jobs::{JobKey, JobState};
use nodecraft_graph::Generation;
use nodecraft_types::{Error, ErrorKind, GenerationId, Modality, Value};
use serde_json::json;

impl RunHost {
    /// Records what the ended job `key` streamed of its text answer on its card, as one undo step.
    pub fn keep_partial(&mut self, session: &mut Session, key: JobKey) -> Result<(), Error> {
        let gone = || Error::new(ErrorKind::NotFound, "that job's partial answer is no longer kept");
        let view =
            self.recent.iter_mut().find(|view| view.key == key && matches!(view.state, JobState::Cancelled | JobState::Failed { .. })).ok_or_else(gone)?;
        let text = view.partial.as_deref().filter(|text| !text.is_empty()).ok_or_else(gone)?.clone();
        let node = session.document().canvases().get(&view.canvas).and_then(|canvas| canvas.nodes().get(&view.node));
        let node = node.ok_or_else(|| Error::new(ErrorKind::NotFound, "the card the answer was for is gone"))?;
        if !matches!(node.outputs.as_slice(), [port] if port.port_type.modality == Modality::Text && !port.port_type.many) {
            return Err(Error::new(ErrorKind::Unsupported, "only a card with one text output can keep a partial answer"));
        }
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|_| Error::new(ErrorKind::Internal, "no entropy for the kept answer's id"))?;
        let generation = Generation::new(GenerationId::from_bytes(id), vec![Value::Text(text)]);
        let params = json!({"canvas": view.canvas, "results": [{"node": view.node, "generation": generation, "keep": true}]});
        session.dispatch("run.record", &params)?;
        view.partial = None;
        Ok(())
    }

    /// Lets go of what the ended job `key` streamed, without recording it.
    pub fn discard_partial(&mut self, key: JobKey) {
        if let Some(view) = self.recent.iter_mut().find(|view| view.key == key) {
            view.partial = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MAX_KEPT_PARTIALS, keep_recent};
    use crate::run_jobs::{JobKey, JobState, JobView};
    use nodecraft_exec::scheduler::JobId;
    use nodecraft_types::{CanvasId, NodeId};
    use std::collections::VecDeque;
    use std::sync::Arc;

    fn stopped(job: u64, node: u8, text: &str) -> JobView {
        JobView {
            key: JobKey { run: 1, job: JobId(job) },
            canvas: CanvasId::from_bytes([1; 16]),
            node: NodeId::from_bytes([node; 16]),
            provider: Some("mock".into()),
            model: Some("m".into()),
            estimate: None,
            state: JobState::Cancelled,
            sent: true,
            cancel_requested: false,
            cancel_refused: false,
            reported_sent: true,
            partial: Some(Arc::new(text.to_owned())),
            partial_attempt: 1,
        }
    }

    #[test]
    fn only_the_newest_stopped_answer_of_a_node_is_kept_and_all_of_them_within_a_budget() {
        let mut recent = VecDeque::new();
        keep_recent(&mut recent, [stopped(1, 1, "old"), stopped(2, 2, "other")].into_iter());
        keep_recent(&mut recent, std::iter::once(stopped(3, 1, "new")));
        let kept: Vec<_> = recent.iter().map(|v| v.partial.as_deref().map(String::as_str)).collect();
        assert_eq!(kept, [None, Some("other"), Some("new")]);

        let big = "x".repeat(nodecraft_providers::MAX_STREAMED_TEXT);
        let many = MAX_KEPT_PARTIALS / big.len() + 2;
        keep_recent(&mut recent, (0..many).map(|n| stopped(10 + n as u64, 10 + n as u8, &big)));
        let total: usize = recent.iter().filter_map(|v| v.partial.as_ref()).map(|t| t.len()).sum();
        assert!(total <= MAX_KEPT_PARTIALS, "{total}");
        assert!(recent.back().is_some_and(|v| v.partial.is_some()), "the newest are kept");
    }
}
