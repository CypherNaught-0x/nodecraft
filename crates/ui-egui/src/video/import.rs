//! Best-effort metadata before a file is published. Reuses the bounded playback service;
//! dropping a form/drop cancels its job, and every wait has a short frontend deadline.
use super::Videos;
use nodecraft_engine::import::ProbedMovie;
use nodecraft_media::video::{VideoAsset, VideoJob};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const WAIT: Duration = Duration::from_secs(3);

pub(crate) struct Probe {
    bytes: Arc<[u8]>,
    job: Option<VideoJob<VideoAsset>>,
    started: Instant,
}

impl Probe {
    pub(crate) fn new(bytes: Arc<[u8]>) -> Option<Self> {
        let sniffed = nodecraft_media::sniff(&bytes)?;
        if sniffed.kind != nodecraft_media::MediaKind::Video {
            return None;
        }
        if nodecraft_media::probe_movie(&bytes).is_ok_and(|movie| movie.duration_ms.is_some() && movie.width.is_some() && movie.height.is_some()) {
            return None;
        }
        Some(Self { bytes, job: None, started: Instant::now() })
    }

    /// `None` is pending; `Some(None)` leaves metadata unknown and still imports the file.
    pub(crate) fn poll(&mut self, videos: &mut Videos, ctx: &egui::Context) -> Option<Option<ProbedMovie>> {
        if self.started.elapsed() >= WAIT {
            self.job = None;
            return Some(None);
        }
        if self.job.is_none() {
            match videos.probe_import(ctx, Arc::clone(&self.bytes)) {
                None => return None,
                Some(Err(())) => return Some(None),
                Some(Ok(job)) => self.job = Some(job),
            }
        }
        match self.job.as_mut()?.try_result() {
            Ok(None) => None,
            Ok(Some(asset)) => Some(Some(ProbedMovie::from_video(&asset))),
            Err(_) => Some(None),
        }
    }
}

#[cfg(test)]
mod tests;
