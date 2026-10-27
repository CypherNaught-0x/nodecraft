use super::ImageError;
use std::sync::Arc;

/// Synchronous executor with an explicit bounded pool; no global rayon initialization.
#[derive(Default)]
pub struct CpuExecutor {
    #[cfg(not(target_arch = "wasm32"))]
    pool: Option<rayon::ThreadPool>,
    work_check: Option<Arc<dyn Fn(u64) -> bool + Send + Sync>>,
}
impl CpuExecutor {
    pub fn serial() -> Self {
        Self::default()
    }
    /// Checks a caller-owned cancellation signal between bounded pixel chunks and passes.
    /// A stopped operation returns an error; incomplete pixels are never a successful image.
    pub fn with_cancel_check(mut self, check: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.work_check = Some(Arc::new(move |_| check()));
        self
    }
    /// Admit the reference work once before execution, then poll with zero units at
    /// cooperative boundaries. Returning true refuses incomplete output.
    pub fn with_work_check(mut self, check: Arc<dyn Fn(u64) -> bool + Send + Sync>) -> Self {
        self.work_check = Some(check);
        self
    }
    pub(super) fn admit_work(&self, units: u64) -> Result<(), ImageError> {
        if self.work_check.as_ref().is_some_and(|check| check(units)) { Err(ImageError::Cancelled) } else { Ok(()) }
    }
    pub(super) fn check_cancelled(&self) -> Result<(), ImageError> {
        self.admit_work(0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn parallel(threads: usize) -> Result<Self, ImageError> {
        if !(1..=8).contains(&threads) {
            return Err(ImageError::Parameter("image.executor.threads"));
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().map_err(|_| ImageError::Executor)?;
        Ok(Self { pool: Some(pool), ..Default::default() })
    }
    pub(super) fn pixels(&self, out: &mut [u8], width: u32, paint: impl Fn(u32, u32) -> [u8; 4] + Sync + Send) -> Result<(), ImageError> {
        self.check_cancelled()?;
        let width = usize::try_from(width).ok().filter(|width| *width > 0).ok_or(ImageError::Dimensions)?;
        let draw = |chunk: usize, pixels: &mut [u8]| -> Result<(), ImageError> {
            self.check_cancelled()?;
            let start = chunk.checked_mul(256).ok_or(ImageError::Dimensions)?;
            for (offset, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let index = start.saturating_add(offset);
                pixel.copy_from_slice(&paint((index % width) as u32, (index / width) as u32));
            }
            Ok(())
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(pool) = &self.pool {
            use rayon::prelude::*;
            pool.install(|| out.par_chunks_mut(256 * 4).enumerate().try_for_each(|(index, pixels)| draw(index, pixels)))?;
            return self.check_cancelled();
        }
        for (index, pixels) in out.chunks_mut(256 * 4).enumerate() {
            draw(index, pixels)?;
        }
        self.check_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_pixel_pass_stops_early_and_never_reports_partial_success() {
        let checks = Arc::new(AtomicUsize::new(0));
        let seen = checks.clone();
        let executor = CpuExecutor::serial().with_cancel_check(Arc::new(move || seen.fetch_add(1, Ordering::Relaxed) >= 3));
        let painted = AtomicUsize::new(0);
        let mut out = vec![0; 1024 * 1024 * 4];
        assert_eq!(
            executor.pixels(&mut out, 1024, |_, _| {
                painted.fetch_add(1, Ordering::Relaxed);
                [255; 4]
            }),
            Err(ImageError::Cancelled)
        );
        assert!(painted.load(Ordering::Relaxed) > 0 && painted.load(Ordering::Relaxed) < 1024 * 1024);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_parallel_pixel_pass_observes_its_owners_cancel() {
        let checks = Arc::new(AtomicUsize::new(0));
        let seen = checks.clone();
        let executor = CpuExecutor::parallel(2).unwrap().with_cancel_check(Arc::new(move || seen.fetch_add(1, Ordering::Relaxed) >= 3));
        let painted = AtomicUsize::new(0);
        let mut out = vec![0; 1024 * 1024 * 4];
        assert_eq!(
            executor.pixels(&mut out, 1024, |_, _| {
                painted.fetch_add(1, Ordering::Relaxed);
                [255; 4]
            }),
            Err(ImageError::Cancelled)
        );
        assert!(painted.load(Ordering::Relaxed) < 1024 * 1024);
    }
    #[test]
    fn reference_work_is_admitted_before_pixel_execution() {
        use crate::image::{ImageInputs, ImageLimits, ImageRequest, Raster, params::Axis};
        let source = Raster::new(8, 8, vec![255; 8 * 8 * 4], ImageLimits::default()).unwrap();
        let admitted = Arc::new(AtomicUsize::new(0));
        let observed = admitted.clone();
        let mut executor = CpuExecutor::serial().with_work_check(Arc::new(move |units| {
            if units > 0 {
                observed.store(usize::try_from(units).unwrap(), Ordering::Relaxed);
                true
            } else {
                false
            }
        }));
        assert!(matches!(
            ImageRequest::Flip { axis: Axis::Horizontal }.execute(ImageInputs::single(&source), &mut executor, ImageLimits::default()),
            Err(ImageError::Cancelled)
        ));
        assert!(admitted.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn separable_blurs_poll_during_their_manual_fill_pass() {
        use crate::image::{ImageInputs, ImageLimits, ImageRequest, Raster, params::Blur};
        let source = Raster::new(256, 256, vec![255; 256 * 256 * 4], ImageLimits::default()).unwrap();
        for options in [Blur::Gaussian { sigma: 10.0 }, Blur::Box { radius: 10 }] {
            let checks = Arc::new(AtomicUsize::new(0));
            let observed = checks.clone();
            let mut executor = CpuExecutor::serial().with_cancel_check(Arc::new(move || observed.fetch_add(1, Ordering::Relaxed) >= 3));
            assert!(matches!(
                ImageRequest::Blur { options }.execute(ImageInputs::single(&source), &mut executor, ImageLimits::default()),
                Err(ImageError::Cancelled)
            ));
        }
    }
}
