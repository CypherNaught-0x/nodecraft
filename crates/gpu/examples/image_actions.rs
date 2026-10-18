//! Real headless GPU action with an explicit bounded native progress loop.
#[cfg(target_arch = "wasm32")]
fn main() {}
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use nodecraft_actions::image::params::Blur;
    use nodecraft_actions::image::{ImageInputs, ImageLimits, ImageRequest, ImageResult, Raster};
    use nodecraft_gpu::image::GpuExecutor;
    use std::{
        future::Future,
        task::{Context, Poll},
        time::{Duration, Instant},
    };
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    println!("NodeCraft image GPU: {} ({:?})", gpu.adapter_info().name, gpu.adapter_info().backend);
    let limits = ImageLimits::default();
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(64 * 64 * 4)?;
    for y in 0..64u8 {
        for x in 0..64u8 {
            pixels.extend_from_slice(&[x * 4, y * 4, 128, 255]);
        }
    }
    let source = Raster::new(64, 64, pixels, limits)?;
    let request = ImageRequest::Blur { options: Blur::Gaussian { sigma: 2. } };
    // Acquire BEFORE execute borrows the executor. A GUI host can instead poll its shared Device.
    let handle = gpu.poll_handle();
    let deadline = Instant::now();
    let result = {
        let mut future = std::pin::pin!(gpu.execute(&request, ImageInputs::single(&source), limits));
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        loop {
            if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                break result?;
            }
            handle.poll_nonblocking()?;
            if deadline.elapsed() > Duration::from_secs(10) {
                return Err("image GPU completion deadline exceeded".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let ImageResult::Image(image) = result else { return Err("unexpected image collection".into()) };
    println!("result={}x{} bytes={} counters={:?}", image.width(), image.height(), image.pixels().len(), gpu.counters());
    Ok(())
}
