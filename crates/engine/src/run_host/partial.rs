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
