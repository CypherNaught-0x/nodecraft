//! Real installed-tool consumer: cargo run -p nodecraft-media --features video --example video_roundtrip -- /absolute/ffmpeg /absolute/ffprobe /new/output.webm
#[cfg(all(feature = "video", not(target_arch = "wasm32")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use nodecraft_media::video::*;
    use std::{
        io::Write,
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };
    fn wait<T>(job: &mut VideoJob<T>) -> Result<T, VideoError> {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(value) = job.try_result()? {
                return Ok(value);
            }
            if Instant::now() >= end {
                job.cancel();
                return Err(VideoError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let mut args = std::env::args_os().skip(1);
    let ffmpeg = PathBuf::from(args.next().ok_or("missing absolute ffmpeg path")?);
    let ffprobe = PathBuf::from(args.next().ok_or("missing absolute ffprobe path")?);
    let output = PathBuf::from(args.next().ok_or("missing new output path")?);
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let mut opening = VideoService::begin_open(ServiceConfig::new(ToolPaths::new(ffmpeg, ffprobe)?, std::env::temp_dir(), VideoLimits::default())?)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let service = loop {
        if let Some(service) = opening.try_result()? {
            break service;
        }
        if Instant::now() >= deadline {
            return Err("video startup deadline".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let (mut frames, mut job) = service.render(RenderSpec::new(64, 64, (1, 1), 3, [0, 0, 0])?)?;
    let send_deadline = Instant::now() + Duration::from_secs(30);
    for (i, color) in [[255, 0, 0, 255], [0, 255, 0, 128], [0, 0, 255, 255]].into_iter().enumerate() {
        let bytes: Vec<u8> = std::iter::repeat_n(color, 4096).flatten().collect();
        let mut packet = FramePacket { index: u32::try_from(i)?, rgba: Arc::from(bytes) };
        loop {
            if Instant::now() >= send_deadline {
                return Err(VideoError::Timeout.into());
            }
            match frames.try_send(packet) {
                Ok(()) => break,
                Err(e) if e.error == VideoError::Busy => {
                    packet = e.packet;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.error.into()),
            }
        }
    }
    frames.finish().map_err(|_| "frames incomplete")?;
    let artifact = wait(&mut job)?;
    let mut preview = service.frame(artifact.asset(), FrameRequest::new(1_000_000, 32, 32)?)?;
    let decoded = wait(&mut preview)?;
    println!("verified {} frames; decoded {}x{} RGBA; content {}", artifact.frames(), decoded.width, decoded.height, artifact.hash());
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(output)?;
    let mut offset = 0;
    while offset < artifact.bytes() {
        let length = usize::try_from((artifact.bytes() - offset).min(1024 * 1024))?;
        let mut chunk = service.read_chunk(artifact.asset(), offset, length)?;
        let chunk = wait(&mut chunk)?;
        file.write_all(chunk.bytes())?;
        offset += u64::try_from(length)?;
    }
    file.sync_all()?;
    drop(decoded);
    drop(preview);
    drop(artifact);
    drop(job);
    let ticket = service.begin_shutdown();
    ticket.wait_blocking(Duration::from_secs(10)).map_err(|e| e.error)?;
    Ok(())
}
#[cfg(any(not(feature = "video"), target_arch = "wasm32"))]
fn main() {
    eprintln!("the example requires the video feature on a native host");
}
