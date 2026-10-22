//! One bounded, cancellable metadata pass between acquisition and atomic publication.
use super::native;
use crate::video::{
    Videos,
    import::{Probe, WAIT},
};
use nodecraft_engine::import::ProbedMovie;
use std::{sync::Arc, time::Instant};

pub(super) struct Pending {
    pub batch: native::Batch,
    pub movies: Vec<ProbedMovie>,
    next: usize,
    active: Option<Probe>,
    started: Instant,
}

impl Pending {
    pub(super) fn new(batch: native::Batch) -> Self {
        Self { batch, movies: Vec::new(), next: 0, active: None, started: Instant::now() }
    }

    pub(super) fn poll(&mut self, videos: &mut Videos, ctx: &egui::Context) -> bool {
        if self.started.elapsed() >= WAIT || !self.batch.may_probe() {
            self.active = None;
            return true;
        }
        loop {
            if let Some(probe) = &mut self.active {
                match probe.poll(videos, ctx) {
                    None => return false,
                    Some(movie) => {
                        if let Some(movie) = movie {
                            self.movies.push(movie);
                        }
                        self.active = None;
                    }
                }
            }
            let Some(file) = self.batch.files.get(self.next) else { return true };
            self.next = self.next.saturating_add(1);
            self.active = Probe::new(Arc::clone(&file.bytes));
        }
    }
}
