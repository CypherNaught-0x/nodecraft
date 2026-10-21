//! Captures (#112): a model drawn offscreen, the way the viewer draws it, and read back as RGBA
//! pixels for a photo or a view sheet. Blocking, for the worker thread that computes actions.
//! Every GPU call runs inside error scopes, an error none of them caught marks the capturer
//! broken, and a view that outlasts its time leaves it unusable, so the next capture starts on a
//! fresh one.
use super::{GpuMesh, MeshData, MeshRenderer, MeshUploadError, Target, View};
use crate::scoped;
use futures::FutureExt;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Longest a whole capture (every view of a sheet) may take to draw and read back; callers share
/// it between their views.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);

/// Why a capture failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// No GPU, or none that answered at once.
    Unavailable,
    /// The model can't be put on this GPU.
    Model(MeshUploadError),
    /// The GPU refused the work or was lost.
    Device,
    /// The GPU took longer than the capture's time.
    Busy,
    /// The image couldn't be read back.
    Mapping,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("no GPU is available"),
            Self::Model(why) => why.fmt(f),
            Self::Device => f.write_str("the GPU failed while drawing the model"),
            Self::Busy => write!(f, "the GPU took longer than {} seconds", TIMEOUT.as_secs()),
            Self::Mapping => f.write_str("the drawn image couldn't be read back"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// The output format the renderer is made for: captures never blit, so any will do.
const OUTPUT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A GPU device and the 3D pipelines, for captures.
pub struct MeshCapture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: MeshRenderer,
    /// Lost, broken by an uncaught error, or left busy by a timeout.
    lost: Arc<AtomicBool>,
}

impl MeshCapture {
    /// A capturer on the system's GPU. Native adapters answer at once; one that doesn't, or no
    /// GPU at all, is `Unavailable`.
    pub fn request() -> Result<Self, CaptureError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .now_or_never()
            .ok_or(CaptureError::Unavailable)?
            .map_err(|_| CaptureError::Unavailable)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .now_or_never()
            .ok_or(CaptureError::Unavailable)?
            .map_err(|_| CaptureError::Unavailable)?;
        let lost = Arc::new(AtomicBool::new(false));
        let flag = lost.clone();
        device.set_device_lost_callback(move |_, _| flag.store(true, Ordering::Release));
        let broken = lost.clone();
        device.on_uncaptured_error(Arc::new(move |_| broken.store(true, Ordering::Release)));
        let (renderer, caught) = scoped(&device, || MeshRenderer::new(&device, OUTPUT, 1));
        if caught {
            return Err(CaptureError::Unavailable);
        }
        Ok(Self { device, queue, renderer, lost })
    }

    /// Whether this capturer can't be used any more (a new one is needed).
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// `size` as a capture will draw it: kept within what an offscreen target may be.
    pub fn clamp_size(&self, size: [u32; 2]) -> [u32; 2] {
        self.renderer.clamp_size(size)
    }

    /// Puts `data` on this GPU.
    pub fn upload(&self, data: MeshData<'_>) -> Result<GpuMesh, CaptureError> {
        if self.is_lost() {
            return Err(CaptureError::Device);
        }
        GpuMesh::upload(&self.device, &self.queue, data).map_err(CaptureError::Model)
    }

    /// Images of `size` (clamped with [`Self::clamp_size`]) to draw captures into, reused from
    /// view to view.
    pub fn target(&self, size: [u32; 2]) -> Result<Target, CaptureError> {
        if self.is_lost() {
            return Err(CaptureError::Device);
        }
        self.renderer.target(&self.device, size).ok_or(CaptureError::Device)
    }

    /// Draws `mesh` into `target` as `view` shows it and reads the image back within `timeout`:
    /// `target`'s size, RGBA8 rows top to bottom, sRGB, straight (not premultiplied) alpha.
    /// `view`'s background and line colours must be premultiplied by their alpha.
    pub fn render(&self, mesh: &GpuMesh, target: &Target, view: &View, timeout: std::time::Duration) -> Result<Vec<u8>, CaptureError> {
        if self.is_lost() {
            return Err(CaptureError::Device);
        }
        let [width, height] = target.size;
        let row = u64::from(width) * 4;
        let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let padded = row.div_ceil(align) * align;
        let bytes = padded.checked_mul(u64::from(height)).ok_or(CaptureError::Device)?;
        let padded_row = u32::try_from(padded).map_err(|_| CaptureError::Device)?;
        let device = self.device.clone();
        let ((staging, index, mapped), caught) = scoped(&device, || {
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("NodeCraft 3D capture"),
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("NodeCraft 3D capture") });
            self.renderer.render(&self.queue, &mut encoder, target, mesh, view);
            encoder.copy_texture_to_buffer(
                target.image.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(height) },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            let index = self.queue.submit([encoder.finish()]);
            let mapped = Arc::new(AtomicBool::new(false));
            let flag = mapped.clone();
            staging.slice(..).map_async(wgpu::MapMode::Read, move |result| flag.store(result.is_ok(), Ordering::Release));
            (staging, index, mapped)
        });
        if caught || self.is_lost() {
            self.lost.store(true, Ordering::Release);
            return Err(CaptureError::Device);
        }
        if self.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(timeout) }).is_err() {
            self.lost.store(true, Ordering::Release);
            return Err(CaptureError::Busy);
        }
        if !mapped.load(Ordering::Acquire) {
            return Err(CaptureError::Mapping);
        }
        let range = staging.slice(..).get_mapped_range().map_err(|_| CaptureError::Mapping)?;
        let row = usize::try_from(row).map_err(|_| CaptureError::Mapping)?;
        let padded = usize::try_from(padded).map_err(|_| CaptureError::Mapping)?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(row.saturating_mul(usize::try_from(height).map_err(|_| CaptureError::Mapping)?)).map_err(|_| CaptureError::Mapping)?;
        for line in range.chunks(padded) {
            pixels.extend_from_slice(line.get(..row).ok_or(CaptureError::Mapping)?);
        }
        drop(range);
        staging.unmap();
        unpremultiply(&mut pixels);
        Ok(pixels)
    }
}

/// Turns premultiplied pixels straight: the multisample resolve averages linear colour, so a
/// model's edge over a (premultiplied) translucent background comes out scaled by its alpha.
/// Opaque and empty pixels are unchanged.
pub fn unpremultiply(pixels: &mut [u8]) {
    let decode = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let encode = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round().clamp(0.0, 255.0) as u8
    };
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = *pixel;
        if a == 0 || a == 255 {
            continue;
        }
        let alpha = f32::from(a) / 255.0;
        *pixel = [encode(decode(r) / alpha), encode(decode(g) / alpha), encode(decode(b) / alpha), a];
    }
}

#[cfg(test)]
mod tests;
