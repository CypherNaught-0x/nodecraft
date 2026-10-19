//! Running a validated shader on one image: blocking, for the worker thread that computes
//! actions. Each distinct module is compiled once and kept (a few at most).
use super::ValidShader;
use crate::image::GpuImageError;
use futures::FutureExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Most compiled shaders kept.
const MAX_PIPELINES: usize = 16;
/// Longest a shader may run before it counts as failed.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct ShaderRunner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: BTreeMap<String, wgpu::ComputePipeline>,
    lost: Arc<AtomicBool>,
}

impl ShaderRunner {
    /// A runner on the system's GPU. Native adapters answer at once; one that doesn't, or no
    /// GPU at all, is `Unavailable`.
    pub fn request() -> Result<Self, GpuImageError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .now_or_never()
            .ok_or(GpuImageError::Unavailable)?
            .map_err(|_| GpuImageError::Unavailable)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .now_or_never()
            .ok_or(GpuImageError::Unavailable)?
            .map_err(|_| GpuImageError::Unavailable)?;
        let lost = Arc::new(AtomicBool::new(false));
        let flag = lost.clone();
        device.set_device_lost_callback(move |_, _| flag.store(true, Ordering::Release));
        Ok(Self { device, queue, pipelines: BTreeMap::new(), lost })
    }

    /// Whether the device is gone (a new runner is needed).
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    fn pipeline(&mut self, shader: &ValidShader) -> Result<wgpu::ComputePipeline, GpuImageError> {
        if let Some(found) = self.pipelines.get(shader.module()) {
            return Ok(found.clone());
        }
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NodeCraft custom shader"),
            source: wgpu::ShaderSource::Wgsl(shader.module().into()),
        });
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("NodeCraft custom shader"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if scope.pop().now_or_never().flatten().is_some() {
            return Err(GpuImageError::Device);
        }
        if self.pipelines.len() >= MAX_PIPELINES {
            self.pipelines.clear();
        }
        self.pipelines.insert(shader.module().to_owned(), pipeline.clone());
        Ok(pipeline)
    }

    /// Runs `shader` with parameter `values` (its declared order) on `rgba`, `width` × `height`
    /// RGBA8 pixels; the result's pixels.
    pub fn run(&mut self, shader: &ValidShader, values: &[f32], width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, GpuImageError> {
        use wgpu::util::DeviceExt;
        if self.is_lost() {
            return Err(GpuImageError::Device);
        }
        let bytes = u64::from(width).checked_mul(u64::from(height)).and_then(|p| p.checked_mul(4)).ok_or(GpuImageError::Resource)?;
        if width == 0 || height == 0 || u64::try_from(rgba.len()).ok() != Some(bytes) || values.len() != shader.params {
            return Err(GpuImageError::Resource);
        }
        let limits = self.device.limits();
        if bytes > limits.max_storage_buffer_binding_size || bytes > limits.max_buffer_size {
            return Err(GpuImageError::Resource);
        }
        let pipeline = self.pipeline(shader)?;
        let mut uniform = [0u8; 80];
        let words = [width, height, 0, 0].into_iter().map(u32::to_le_bytes).chain(values.iter().take(16).map(|v| v.to_le_bytes()));
        for (chunk, word) in uniform.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            chunk.copy_from_slice(&word);
        }
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let src = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: rgba, usage: wgpu::BufferUsages::STORAGE });
        let dst = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &uniform, usage: wgpu::BufferUsages::UNIFORM });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: dst.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(&dst, 0, &staging, 0, bytes);
        let index = self.queue.submit([encoder.finish()]);
        let mapped = Arc::new(AtomicBool::new(false));
        let flag = mapped.clone();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| flag.store(result.is_ok(), Ordering::Release));
        if scope.pop().now_or_never().flatten().is_some() {
            return Err(GpuImageError::Device);
        }
        self.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(TIMEOUT) }).map_err(|_| GpuImageError::Device)?;
        if !mapped.load(Ordering::Acquire) {
            return Err(GpuImageError::Mapping);
        }
        let view = staging.slice(..).get_mapped_range().map_err(|_| GpuImageError::Mapping)?;
        let out = view.to_vec();
        drop(view);
        staging.unmap();
        Ok(out)
    }
}
