//! Async image compute. Native hosts drive a pre-acquired `GpuPollHandle` while awaiting.
use nodecraft_actions::image::params::*;
use nodecraft_actions::image::{ImageError, ImageInputs, ImageLimits, ImageRequest, ImageResult};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

mod kernel;
mod stripe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuImageError {
    Action(ImageError),
    Unavailable,
    Resource,
    Device,
    Mapping,
    Busy,
    Unsupported,
}
impl From<ImageError> for GpuImageError {
    fn from(e: ImageError) -> Self {
        Self::Action(e)
    }
}
impl std::fmt::Display for GpuImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Action(_) => "image action admission failed",
            Self::Unavailable => "no compatible image GPU available",
            Self::Resource => "image GPU resource budget exceeded",
            Self::Device => "image GPU device failed",
            Self::Mapping => "image GPU readback failed",
            Self::Busy => "image GPU submission still pending",
            Self::Unsupported => "image GPU operation not yet available",
        })
    }
}
impl std::error::Error for GpuImageError {}
/// Adapter selection always obtains a fresh adapter; it never imports an unproven device pair.
#[derive(Debug, Clone, Copy)]
pub struct GpuOptions {
    pub backends: wgpu::Backends,
    pub power_preference: wgpu::PowerPreference,
    pub force_fallback_adapter: bool,
}
impl Default for GpuOptions {
    fn default() -> Self {
        Self { backends: wgpu::Backends::all(), power_preference: wgpu::PowerPreference::None, force_fallback_adapter: false }
    }
}
#[derive(Clone)]
pub struct GpuPollHandle {
    device: wgpu::Device,
}
impl GpuPollHandle {
    pub fn device(&self) -> wgpu::Device {
        self.device.clone()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn poll_nonblocking(&self) -> Result<(), GpuImageError> {
        self.device.poll(wgpu::PollType::Poll).map(|_| ()).map_err(|_| GpuImageError::Device)
    }
}
#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    pub decision_visits: u64,
    pub coefficient_visits: u64,
    pub packing_words: u64,
    pub planning_candidates: u64,
    pub encoded_horizontal_rows: u64,
    pub last_stripe_height: u32,
    pub last_float_bytes: u64,
    pub last_peak_bytes: u64,
    pub last_work: u64,
    pub uploaded_bytes: u64,
    pub dispatches: u64,
    pub readbacks: u64,
}
struct Pending {
    buffers: Vec<wgpu::Buffer>,
    complete: Arc<AtomicBool>,
    mapped: Arc<AtomicBool>,
}
pub struct GpuExecutor {
    max_stripe_height: u32,
    #[cfg(test)]
    invalid_submission: bool,
    #[cfg(test)]
    invalid_map: bool,
    #[cfg(test)]
    invalid_upload: bool,
    #[cfg(test)]
    cancel_map: bool,
    adapter: wgpu::AdapterInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    pending: Option<Pending>,
    failed: bool,
    lost: Arc<AtomicBool>,
    counters: Counters,
}
impl GpuExecutor {
    pub async fn request() -> Result<Self, GpuImageError> {
        Self::request_with_options(GpuOptions::default()).await
    }
    pub async fn request_with_options(options: GpuOptions) -> Result<Self, GpuImageError> {
        let backends = options.backends & wgpu::Instance::enabled_backend_features();
        if backends.is_empty() {
            return Err(GpuImageError::Unavailable);
        }
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = backends;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: options.power_preference,
                force_fallback_adapter: options.force_fallback_adapter,
                ..Default::default()
            })
            .await
            .map_err(|_| GpuImageError::Unavailable)?;
        Self::request_on(&adapter).await
    }
    // Only our freshly acquired adapter reaches this constructor. The underlying API documents
    // a caller-history precondition; public arbitrary adapters would reintroduce that ambiguity.
    async fn request_on(adapter: &wgpu::Adapter) -> Result<Self, GpuImageError> {
        let descriptor = wgpu::DeviceDescriptor { required_features: wgpu::Features::empty(), required_limits: wgpu::Limits::default(), ..Default::default() };
        let limits = adapter.limits();
        if !adapter.features().contains(descriptor.required_features)
            || !descriptor.required_limits.check_limits(&limits)
            || limits.max_storage_buffers_per_shader_stage < 7
            || limits.max_uniform_buffers_per_shader_stage < 1
            || limits.max_uniform_buffer_binding_size < 32
            || limits.max_compute_invocations_per_workgroup < 64
            || limits.max_compute_workgroup_size_x < 8
            || limits.max_compute_workgroup_size_y < 8
        {
            return Err(GpuImageError::Resource);
        }
        let (device, queue) = adapter.request_device(&descriptor).await.map_err(|_| GpuImageError::Unavailable)?;
        Self::from_device(device, queue, adapter.get_info()).await
    }
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter
    }
    pub fn device(&self) -> wgpu::Device {
        self.device.clone()
    }
    pub fn queue(&self) -> wgpu::Queue {
        self.queue.clone()
    }
    async fn from_device(device: wgpu::Device, queue: wgpu::Queue, adapter: wgpu::AdapterInfo) -> Result<Self, GpuImageError> {
        if device.limits().max_storage_buffers_per_shader_stage < 7
            || device.limits().max_uniform_buffers_per_shader_stage < 1
            || device.limits().max_uniform_buffer_binding_size < 32
            || device.limits().max_compute_invocations_per_workgroup < 64
            || device.limits().max_compute_workgroup_size_x < 8
            || device.limits().max_compute_workgroup_size_y < 8
        {
            return Err(GpuImageError::Resource);
        }
        let lost = Arc::new(AtomicBool::new(false));
        let callback_lost = lost.clone();
        device.set_device_lost_callback(move |_, _| {
            callback_lost.store(true, Ordering::Release);
        });
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NodeCraft image"),
            source: wgpu::ShaderSource::Wgsl(include_str!("image.wgsl").into()),
        });
        let entries: Vec<_> = (0..8)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: if binding == 7 { wgpu::BufferBindingType::Uniform } else { wgpu::BufferBindingType::Storage { read_only: binding < 4 } },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &entries });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bind_layout)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("NodeCraft image"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let validation = scope.pop();
        let internal = internal.pop();
        let oom = oom.pop();
        let errors = [validation.await, internal.await, oom.await];
        #[cfg(test)]
        for error in errors.iter().flatten() {
            eprintln!("GPU pipeline diagnostic: {error}");
        }
        if errors.iter().any(Option::is_some) {
            return Err(GpuImageError::Device);
        }
        Ok(Self {
            #[cfg(test)]
            invalid_submission: false,
            #[cfg(test)]
            invalid_map: false,
            #[cfg(test)]
            invalid_upload: false,
            #[cfg(test)]
            cancel_map: false,
            max_stripe_height: 128,
            adapter,
            device,
            queue,
            pipeline,
            pending: None,
            failed: false,
            lost,
            counters: Counters::default(),
        })
    }
    pub fn poll_handle(&self) -> GpuPollHandle {
        GpuPollHandle { device: self.device.clone() }
    }
    // All callers hold the device's Validation/Internal/OOM scopes. Unlike the convenience
    // initializer, failed initial mapping is a Result, never an expect inside a dependency.
    fn upload(&mut self, bytes: &[u8], usage: wgpu::BufferUsages) -> Result<wgpu::Buffer, GpuImageError> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
            return Err(GpuImageError::Resource);
        }
        let size = u64::try_from(bytes.len()).map_err(|_| GpuImageError::Resource)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage,
            mapped_at_creation: {
                #[cfg(test)]
                {
                    !self.invalid_upload
                }
                #[cfg(not(test))]
                {
                    true
                }
            },
        });
        {
            let mut mapped = match buffer.get_mapped_range_mut(..) {
                Ok(mapped) => mapped,
                Err(_) => {
                    self.failed = true;
                    return Err(GpuImageError::Device);
                }
            };
            if mapped.len() != bytes.len() {
                self.failed = true;
                return Err(GpuImageError::Device);
            }
            mapped.copy_from_slice(bytes);
        }
        self.counters.uploaded_bytes = self.counters.uploaded_bytes.saturating_add(size);
        buffer.unmap();
        Ok(buffer)
    }
    fn release_mapping(&self, pending: &Pending) {
        if !pending.mapped.swap(false, Ordering::AcqRel) {
            return;
        }
        if let Some(staging) = pending.buffers.last() {
            // Cleanup cannot await; captured cleanup failures are discarded rather than
            // reaching wgpu's default uncaptured-error panic handler.
            let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
            let internal = self.device.push_error_scope(wgpu::ErrorFilter::Internal);
            let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            staging.unmap();
            drop(validation.pop());
            drop(internal.pop());
            drop(oom.pop());
        }
    }
    pub fn counters(&self) -> Counters {
        self.counters
    }
    fn reclaim(&mut self) -> Result<(), GpuImageError> {
        if self.failed || self.lost.load(Ordering::Acquire) {
            self.failed = true;
            return Err(GpuImageError::Device);
        }
        if let Some(p) = &self.pending
            && !p.complete.load(Ordering::Acquire)
        {
            return Err(GpuImageError::Busy);
        }
        if let Some(p) = self.pending.take() {
            self.release_mapping(&p);
        }
        Ok(())
    }
    pub async fn execute(&mut self, request: &ImageRequest, inputs: ImageInputs<'_>, limits: ImageLimits) -> Result<ImageResult, GpuImageError> {
        self.reclaim()?;
        let prepared = request.prepare(inputs, limits)?;
        if prepared.identity() {
            return Ok(ImageResult::Image(inputs.source.clone()));
        }
        if let ImageRequest::Duplicate { count } = request {
            let mut out = Vec::new();
            out.try_reserve_exact(*count as usize).map_err(|_| GpuImageError::Resource)?;
            for _ in 0..*count {
                out.push(inputs.source.clone());
            }
            return Ok(ImageResult::Images(out));
        }
        let layout = prepared.layout();
        let count = u64::from(layout.width) * u64::from(layout.height);
        let resources = kernel::select(&prepared, &self.device.limits(), self.max_stripe_height)?;
        self.counters.planning_candidates = self.counters.planning_candidates.saturating_add(u64::from(resources.candidates));
        self.counters.last_stripe_height = resources.stripes.map_or(0, |s| s.block);
        self.counters.last_float_bytes = resources.float_bytes;
        self.counters.last_peak_bytes = prepared.reference_working_bytes()?.checked_add(resources.extra_bytes).ok_or(GpuImageError::Resource)?;
        self.counters.last_work = prepared.reference_work()?.checked_add(resources.extra_work).ok_or(GpuImageError::Resource)?;
        let output_bytes = count.checked_mul(4).ok_or(GpuImageError::Resource)?;
        let dl = self.device.limits();
        let groups = layout.width.div_ceil(8);
        if groups > dl.max_compute_workgroups_per_dimension || layout.height.div_ceil(8) > dl.max_compute_workgroups_per_dimension {
            return Err(GpuImageError::Resource);
        }
        let packed = kernel::pack(&prepared, &resources)?;
        self.counters.encoded_horizontal_rows = self.counters.encoded_horizontal_rows.saturating_add(resources.stripes.map_or(0, |s| s.sum_rows));
        self.counters.decision_visits = self.counters.decision_visits.saturating_add(packed.visits);
        self.counters.coefficient_visits = self.counters.coefficient_visits.saturating_add(packed.coefficient_visits);
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let source = self.upload(inputs.source.pixels(), wgpu::BufferUsages::STORAGE)?;
        let overlay = self.upload(inputs.overlay.map_or(&[0u8; 4][..], |r| r.pixels()), wgpu::BufferUsages::STORAGE)?;
        let decision = self.upload(&packed.decisions, wgpu::BufferUsages::STORAGE)?;
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: output_bytes,
            usage: {
                #[cfg(test)]
                if self.invalid_map {
                    wgpu::BufferUsages::COPY_DST
                } else {
                    wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ
                }
                #[cfg(not(test))]
                {
                    wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ
                }
            },
            mapped_at_creation: false,
        });
        let float_size = resources.float_bytes;
        let intermediate_size = resources.cover_bytes;
        let storage =
            |size| self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
        let scratch = storage(float_size);
        let intermediate = storage(intermediate_size);
        let mut uniforms = Vec::new();
        let mut groups_for_stages = Vec::new();
        uniforms.try_reserve_exact(resources.stages as usize).map_err(|_| GpuImageError::Resource)?;
        groups_for_stages.try_reserve_exact(resources.stages as usize).map_err(|_| GpuImageError::Resource)?;
        if uniforms.capacity() != resources.stages as usize || groups_for_stages.capacity() != resources.stages as usize {
            return Err(GpuImageError::Resource);
        }
        let mut base = Vec::new();
        base.try_reserve_exact(4096).map_err(|_| GpuImageError::Resource)?;
        if base.capacity() != 4096 {
            return Err(GpuImageError::Resource);
        }
        for word in packed.params {
            base.extend_from_slice(&word.to_le_bytes());
            self.counters.packing_words = self.counters.packing_words.saturating_add(1);
        }
        let uniform = self.upload(&base, wgpu::BufferUsages::STORAGE)?;
        for words in &packed.stages {
            let mut bytes = [0u8; 32];
            for (dst, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                dst.copy_from_slice(&word.to_le_bytes());
                self.counters.packing_words = self.counters.packing_words.saturating_add(1);
            }
            uniforms.push(self.upload(&bytes, wgpu::BufferUsages::UNIFORM)?);
            let control = uniforms.last().ok_or(GpuImageError::Resource)?;
            let bindings = [&source, &overlay, &decision, &uniform, &output, &scratch, &intermediate, control];
            let entries: Vec<_> =
                bindings.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
            groups_for_stages.push(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.pipeline.get_bind_group_layout(0),
                entries: &entries,
            }));
        }
        self.counters.packing_words = self.counters.packing_words.saturating_add(packed.decision_words);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for (group, stage) in groups_for_stages.iter().zip(&packed.stages) {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(groups, stage.get(5).copied().ok_or(GpuImageError::Resource)?.div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, output_bytes);
        #[cfg(test)]
        if self.invalid_submission {
            encoder.copy_buffer_to_buffer(&output, 0, &staging, output_bytes, 4);
        }
        let command = encoder.finish();
        let mut retained = Vec::new();
        retained.try_reserve_exact(8 + uniforms.len()).map_err(|_| GpuImageError::Resource)?;
        if retained.capacity() != 8 + uniforms.len() {
            return Err(GpuImageError::Resource);
        }
        retained.extend([source.clone(), overlay.clone(), decision.clone(), uniform.clone(), output.clone(), scratch.clone(), intermediate.clone()]);
        retained.extend(uniforms.iter().cloned());
        retained.push(staging.clone());
        self.queue.submit([command]);
        // Submission AND map registration stay inside every scope.
        let complete = Arc::new(AtomicBool::new(false));
        let callback_complete = complete.clone();
        let mapped = Arc::new(AtomicBool::new(false));
        let callback_mapped = mapped.clone();
        let (tx, rx) = futures::channel::oneshot::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            callback_mapped.store(result.is_ok(), Ordering::Release);
            callback_complete.store(true, Ordering::Release);
            let _ = tx.send(result);
        });
        #[cfg(test)]
        if self.cancel_map {
            staging.unmap();
        }
        self.pending = Some(Pending { buffers: retained, complete, mapped });
        self.counters.dispatches = self.counters.dispatches.saturating_add(u64::from(resources.stages));

        // All synchronous fallible GPU API calls and owned Pending registration precede yield.
        let validation = scope.pop();
        let internal = internal.pop();
        let oom = oom.pop();
        if validation.await.is_some() || internal.await.is_some() || oom.await.is_some() {
            self.failed = true;
            return Err(GpuImageError::Device);
        }
        if rx.await.map_err(|_| GpuImageError::Mapping)?.is_err() {
            self.failed = true;
            return Err(GpuImageError::Mapping);
        }
        let pending = self.pending.as_ref().ok_or(GpuImageError::Mapping)?;
        let staging = pending.buffers.last().ok_or(GpuImageError::Mapping)?;
        let mapped = staging.slice(..).get_mapped_range().map_err(|_| GpuImageError::Mapping)?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(mapped.len()).map_err(|_| GpuImageError::Resource)?;
        if pixels.capacity() != mapped.len() {
            return Err(GpuImageError::Resource);
        }
        pixels.extend_from_slice(&mapped);
        drop(mapped);
        self.release_mapping(pending);
        self.pending = None;
        self.counters.readbacks = self.counters.readbacks.saturating_add(1);
        Ok(ImageResult::Image(prepared.finish(pixels)?))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

impl Drop for GpuExecutor {
    fn drop(&mut self) {
        if let Some(p) = self.pending.take() {
            self.release_mapping(&p);
        }
    }
}
