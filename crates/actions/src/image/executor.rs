#[cfg(not(target_arch = "wasm32"))]
use super::ImageError;

/// Synchronous executor with an explicit bounded pool; no global rayon initialization.
#[derive(Default)]
pub struct CpuExecutor {
    #[cfg(not(target_arch = "wasm32"))]
    pool: Option<rayon::ThreadPool>,
}
impl CpuExecutor {
    pub fn serial() -> Self {
        Self::default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn parallel(threads: usize) -> Result<Self, ImageError> {
        if !(1..=8).contains(&threads) {
            return Err(ImageError::Parameter("image.executor.threads"));
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().map_err(|_| ImageError::Executor)?;
        Ok(Self { pool: Some(pool) })
    }
    pub(super) fn pixels(&mut self, out: &mut [u8], width: u32, paint: impl Fn(u32, u32) -> [u8; 4] + Sync + Send) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(pool) = &self.pool {
            use rayon::prelude::*;
            pool.install(|| {
                out.par_chunks_exact_mut(4).enumerate().for_each(|(i, pixel)| {
                    let w = width as usize;
                    pixel.copy_from_slice(&paint((i % w) as u32, (i / w) as u32));
                })
            });
            return;
        }
        for (i, pixel) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let w = width as usize;
            pixel.copy_from_slice(&paint((i % w) as u32, (i / w) as u32));
        }
    }
}
