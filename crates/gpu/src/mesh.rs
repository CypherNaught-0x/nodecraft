//! The 3D preview renderer (#111). A model is uploaded once ([`GpuMesh`]), drawn into a card's
//! own offscreen [`Target`] (multisampled colour and depth, resolved), then copied into the
//! caller's render pass ([`MeshRenderer::blit`]). Rendering offscreen keeps depth testing out of
//! the UI's pass, which has no depth buffer, and leaves a finished image a capture can read (#112).
//!
//! The camera ([`Orbit`]) and the [`Style`] are view state: nothing here touches a document.

use std::fmt;

/// Offscreen captures of a model into image pixels (#112).
pub mod capture;

/// Bytes per vertex: position (3 × f32), normal (3 × f32), linear RGBA colour (4 × f32).
pub const VERTEX_BYTES: u64 = 40;
/// Multisampling of the offscreen pass.
const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The longest side of an offscreen target, whatever the device allows.
pub const MAX_TARGET_SIDE: u32 = 4096;
/// Most pixels in one offscreen target; a larger one is drawn smaller and scaled up, keeping its
/// shape. With multisampled colour and depth a pixel costs [`TARGET_PIXEL_BYTES`].
pub const MAX_TARGET_PIXELS: u64 = 2048 * 2048;
/// GPU bytes per target pixel: 4× multisampled colour and depth, and the resolved image.
pub const TARGET_PIXEL_BYTES: u64 = 4 * SAMPLES as u64 + 4 * SAMPLES as u64 + 4;
/// Vertical field of view of the camera, in radians.
const FOV: f32 = 0.7;
/// Bytes of the per-target uniform block: a 4×4 matrix, the light and the line colour.
const UNIFORM_BYTES: u64 = 96;

/// Why a model couldn't be put on the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshUploadError {
    /// No triangles, or per-vertex lists of different lengths, or an index out of range.
    Invalid,
    /// Larger than the device's buffers allow.
    TooLarge,
}

impl fmt::Display for MeshUploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "the model has nothing to draw",
            Self::TooLarge => "the model is too large for this GPU",
        })
    }
}

impl std::error::Error for MeshUploadError {}

/// A model's geometry, as the renderer reads it. `edges` (pairs of vertex indices) is what the
/// wireframe style draws; without it the wireframe falls back to the shaded model.
#[derive(Clone, Copy, Debug)]
pub struct MeshData<'a> {
    pub positions: &'a [[f32; 3]],
    pub normals: &'a [[f32; 3]],
    pub colors: &'a [[f32; 4]],
    pub indices: &'a [u32],
    pub edges: Option<&'a [u32]>,
}

/// A model's buffers on the GPU.
pub struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    edges: Option<(wgpu::Buffer, u32)>,
    bytes: u64,
}

impl GpuMesh {
    /// Uploads `data`, checking it against itself and the device's limits first.
    pub fn upload(device: &wgpu::Device, queue: &wgpu::Queue, data: MeshData<'_>) -> Result<Self, MeshUploadError> {
        let n = data.positions.len();
        if n == 0 || data.normals.len() != n || data.colors.len() != n || data.indices.is_empty() || !data.indices.len().is_multiple_of(3) {
            return Err(MeshUploadError::Invalid);
        }
        let in_range = |list: &[u32]| list.iter().all(|&i| usize::try_from(i).is_ok_and(|i| i < n));
        if !in_range(data.indices) || data.edges.is_some_and(|e| !e.len().is_multiple_of(2) || !in_range(e)) {
            return Err(MeshUploadError::Invalid);
        }
        let max = device.limits().max_buffer_size;
        let vertex_bytes = u64::try_from(n).ok().and_then(|n| n.checked_mul(VERTEX_BYTES)).filter(|&b| b <= max).ok_or(MeshUploadError::TooLarge)?;
        let index_bytes = |list: &[u32]| u64::try_from(list.len()).ok().and_then(|n| n.checked_mul(4)).filter(|&b| b <= max).ok_or(MeshUploadError::TooLarge);
        let index_count = u32::try_from(data.indices.len()).map_err(|_| MeshUploadError::TooLarge)?;
        let mut interleaved: Vec<u8> = Vec::new();
        interleaved.try_reserve_exact(usize::try_from(vertex_bytes).map_err(|_| MeshUploadError::TooLarge)?).map_err(|_| MeshUploadError::TooLarge)?;
        for ((p, normal), c) in data.positions.iter().zip(data.normals).zip(data.colors) {
            for v in p.iter().chain(normal).chain(c) {
                interleaved.extend_from_slice(&v.to_le_bytes());
            }
        }
        let buffer = |label: &str, bytes: &[u8], size: u64, usage: wgpu::BufferUsages| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytes);
            buffer
        };
        let words = |list: &[u32]| list.iter().flat_map(|i| i.to_le_bytes()).collect::<Vec<u8>>();
        let triangle_bytes = index_bytes(data.indices)?;
        let edge_list = match data.edges {
            Some(list) if !list.is_empty() => Some((list, index_bytes(list)?, u32::try_from(list.len()).map_err(|_| MeshUploadError::TooLarge)?)),
            _ => None,
        };
        // A GPU out of memory refuses the model rather than reaching the device's error handler.
        let (mesh, caught) = crate::scoped(device, || {
            let vertices = buffer("NodeCraft 3D vertices", &interleaved, vertex_bytes, wgpu::BufferUsages::VERTEX);
            let indices = buffer("NodeCraft 3D triangles", &words(data.indices), triangle_bytes, wgpu::BufferUsages::INDEX);
            let edges = edge_list.map(|(list, size, count)| (buffer("NodeCraft 3D edges", &words(list), size, wgpu::BufferUsages::INDEX), count));
            let bytes = vertex_bytes.saturating_add(triangle_bytes).saturating_add(edge_list.map_or(0, |(_, size, _)| size));
            Self { vertices, indices, index_count, edges, bytes }
        });
        if caught {
            return Err(MeshUploadError::TooLarge);
        }
        Ok(mesh)
    }

    /// GPU memory the model's buffers take.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Whether the wireframe style can draw this model's edges.
    pub fn has_edges(&self) -> bool {
        self.edges.is_some()
    }
}

/// How the model is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Style {
    /// Lit by a light that follows the camera.
    #[default]
    Shaded,
    /// Its colours as they are, without light.
    Unlit,
    /// The triangles' edges only.
    Wireframe,
}

/// The camera, orbiting the model's centre. `yaw` and `pitch` are radians; `zoom` 1 fits the
/// whole model, larger is closer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Orbit {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
}

impl Orbit {
    /// The most the camera tilts above or below the model, short of straight down (where the up
    /// direction is undefined).
    pub const MAX_PITCH: f32 = 1.55;
    pub const MIN_ZOOM: f32 = 0.25;
    pub const MAX_ZOOM: f32 = 16.0;

    /// This orbit turned by a drag of (`dx`, `dy`) radians, kept in range.
    pub fn turned(self, dx: f32, dy: f32) -> Self {
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let yaw = (self.yaw + finite(dx)).rem_euclid(std::f32::consts::TAU);
        Self { yaw, pitch: (self.pitch + finite(dy)).clamp(-Self::MAX_PITCH, Self::MAX_PITCH), zoom: self.zoom }
    }

    /// This orbit zoomed by `factor` (above 1 is closer), kept in range.
    pub fn zoomed(self, factor: f32) -> Self {
        let factor = if factor.is_finite() && factor > 0.0 { factor } else { 1.0 };
        Self { zoom: (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM), ..self }
    }

    /// The camera's position for a model centred on `center` with bounding radius `radius`.
    fn eye(self, center: [f32; 3], radius: f32) -> [f32; 3] {
        let distance = radius / (FOV / 2.0).sin() / self.zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.clamp(-Self::MAX_PITCH, Self::MAX_PITCH).sin_cos();
        [center[0] + distance * cp * sy, center[1] + distance * sp, center[2] + distance * cp * cy]
    }

    /// The column-major view-projection matrix for a viewport of `aspect` (width / height).
    pub fn view_projection(self, center: [f32; 3], radius: f32, aspect: f32) -> [f32; 16] {
        let radius = if radius.is_finite() && radius > 1e-6 { radius } else { 1.0 };
        let aspect = if aspect.is_finite() && aspect > 1e-3 { aspect } else { 1.0 };
        let eye = self.eye(center, radius);
        let distance = length(sub(eye, center));
        let near = (distance - radius * 1.5).max(distance * 0.01);
        let far = distance + radius * 1.5;
        multiply(&perspective(FOV, aspect, near, far), &look_at(eye, center))
    }

    /// The direction towards the light: above and to the left of the camera, so it follows the
    /// camera round the model.
    fn light(self) -> [f32; 3] {
        let lit = Self { yaw: self.yaw - 0.6, pitch: (self.pitch + 0.6).min(Self::MAX_PITCH), zoom: 1.0 };
        normalize(lit.eye([0.0; 3], 1.0))
    }
}

impl Default for Orbit {
    /// A three-quarter view from slightly above.
    fn default() -> Self {
        Self { yaw: 0.6, pitch: 0.35, zoom: 1.0 }
    }
}

/// What one frame of a preview shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub orbit: Orbit,
    pub style: Style,
    /// The model's bounding sphere.
    pub center: [f32; 3],
    pub radius: f32,
    /// Linear RGBA: what's behind the model, and the wireframe's lines.
    pub background: [f32; 4],
    pub line: [f32; 4],
}

/// One card's offscreen images.
pub struct Target {
    size: [u32; 2],
    /// The resolved image's texture, which a capture copies out.
    image: wgpu::Texture,
    color: wgpu::TextureView,
    resolved: wgpu::TextureView,
    depth: wgpu::TextureView,
    uniforms: wgpu::Buffer,
    mesh_group: wgpu::BindGroup,
    blit_group: wgpu::BindGroup,
}

impl Target {
    /// GPU memory its images take.
    pub fn bytes(&self) -> u64 {
        u64::from(self.size[0]).saturating_mul(u64::from(self.size[1])).saturating_mul(TARGET_PIXEL_BYTES)
    }

    /// Its size in pixels.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }
}

/// The pipelines, made once per device.
pub struct MeshRenderer {
    shaded: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    mesh_layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Whether the blit writes to a non-sRGB format, encoding colours itself.
    encode: bool,
    max_side: u32,
    /// The device's largest texture side, which also bounds viewports.
    max_dimension: f32,
}

impl MeshRenderer {
    /// Pipelines for blitting into a pass drawing `output` with `samples` samples per pixel and no
    /// depth buffer (egui's pass).
    pub fn new(device: &wgpu::Device, output: wgpu::TextureFormat, samples: u32) -> Self {
        let mesh_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NodeCraft 3D"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mesh/mesh.wgsl").into()),
        });
        let blit_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NodeCraft 3D blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mesh/blit.wgsl").into()),
        });
        let uniform = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let mesh_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("NodeCraft 3D"),
            entries: &[uniform(0, wgpu::ShaderStages::VERTEX_FRAGMENT)],
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("NodeCraft 3D blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let layout = |group: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(group)], immediate_size: 0 })
        };
        let mesh_pipeline_layout = layout(&mesh_layout);
        let vertex_attributes = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 24, shader_location: 2 },
        ];
        let vertex_buffers =
            [Some(wgpu::VertexBufferLayout { array_stride: VERTEX_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &vertex_attributes })];
        let model = |label: &str, fragment: &str, topology: wgpu::PrimitiveTopology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&mesh_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &mesh_module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &vertex_buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &mesh_module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                // No culling: models whose faces point inwards still show.
                primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
                multiview_mask: None,
                cache: None,
            })
        };
        let shaded = model("NodeCraft 3D shaded", "fs_shaded", wgpu::PrimitiveTopology::TriangleList);
        let lines = model("NodeCraft 3D wireframe", "fs_line", wgpu::PrimitiveTopology::LineList);
        let blit_pipeline_layout = layout(&blit_layout);
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("NodeCraft 3D blit"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState { module: &blit_module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &blit_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: samples.max(1), ..Default::default() },
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("NodeCraft 3D blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let max_dimension = device.limits().max_texture_dimension_2d;
        let max_side = max_dimension.min(MAX_TARGET_SIDE);
        Self { shaded, lines, blit, mesh_layout, blit_layout, sampler, encode: !output.is_srgb(), max_side, max_dimension: max_dimension as f32 }
    }

    /// `size` kept within what a target may be: at least one pixel, at most the device's (and
    /// [`MAX_TARGET_SIDE`]) on a side and [`MAX_TARGET_PIXELS`] in all, scaled down evenly so the
    /// image keeps its shape.
    pub fn clamp_size(&self, size: [u32; 2]) -> [u32; 2] {
        let [w, h] = size.map(|side| f64::from(side.max(1)));
        let side = f64::from(self.max_side.max(1));
        let scale = (side / w).min(side / h).min((MAX_TARGET_PIXELS as f64 / (w * h)).sqrt()).min(1.0);
        [w, h].map(|v| ((v * scale).floor() as u32).clamp(1, self.max_side.max(1)))
    }

    /// Offscreen images of `size` (clamped with [`Self::clamp_size`]); `None` when the GPU
    /// can't make them (out of memory, say).
    pub fn target(&self, device: &wgpu::Device, size: [u32; 2]) -> Option<Target> {
        let (target, caught) = crate::scoped(device, || self.make_target(device, size));
        (!caught).then_some(target)
    }

    fn make_target(&self, device: &wgpu::Device, size: [u32; 2]) -> Target {
        let size = self.clamp_size(size);
        let texture = |label: &str, format, samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let view = |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color = view(&texture("NodeCraft 3D colour", COLOR_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT));
        let image = texture(
            "NodeCraft 3D image",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let resolved = view(&image);
        let depth = view(&texture("NodeCraft 3D depth", DEPTH_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("NodeCraft 3D view"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("NodeCraft 3D output"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: true,
        });
        if let Ok(mut mapped) = output.get_mapped_range_mut(..) {
            let flag = if self.encode { 1.0f32 } else { 0.0 };
            let words: Vec<u8> = [flag, 0.0, 0.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
            mapped.copy_from_slice(&words);
        }
        output.unmap();
        let mesh_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("NodeCraft 3D view"),
            layout: &self.mesh_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let blit_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("NodeCraft 3D blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&resolved) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
            ],
        });
        Target { size, image, color, resolved, depth, uniforms, mesh_group, blit_group }
    }

    /// Draws `mesh` into `target` as `view` shows it.
    pub fn render(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &Target, mesh: &GpuMesh, view: &View) {
        let aspect = target.size[0] as f32 / target.size[1].max(1) as f32;
        let matrix = view.orbit.view_projection(view.center, view.radius, aspect);
        let light = view.orbit.light();
        let unlit = if view.style == Style::Unlit { 1.0 } else { 0.0 };
        let mut block = Vec::with_capacity(UNIFORM_BYTES as usize);
        for v in matrix.iter().chain(&light).chain(&[unlit]).chain(&view.line) {
            block.extend_from_slice(&v.to_le_bytes());
        }
        queue.write_buffer(&target.uniforms, 0, &block);
        let [r, g, b, a] = view.background.map(|c| if c.is_finite() { f64::from(c.clamp(0.0, 1.0)) } else { 0.0 });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("NodeCraft 3D"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.color,
                depth_slice: None,
                resolve_target: Some(&target.resolved),
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }), store: wgpu::StoreOp::Discard },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &target.mesh_group, &[]);
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        match (&mesh.edges, view.style) {
            (Some((edges, count)), Style::Wireframe) => {
                pass.set_pipeline(&self.lines);
                pass.set_index_buffer(edges.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*count, 0, 0..1);
            }
            _ => {
                pass.set_pipeline(&self.shaded);
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }
    }

    /// Copies `target`'s last render into `pass`, stretched over `viewport` (x, y, width, height
    /// in pixels). The viewport may reach past the pass's edges (a card half off screen); the
    /// pass's scissor keeps the drawing inside. `false`, drawing nothing, when the device can't
    /// take such a viewport.
    pub fn blit(&self, pass: &mut wgpu::RenderPass<'_>, target: &Target, viewport: [f32; 4]) -> bool {
        let [x, y, w, h] = viewport;
        let reach = self.max_dimension * 2.0;
        let valid = viewport.iter().all(|v| v.is_finite())
            && w > 0.0
            && h > 0.0
            && w <= self.max_dimension
            && h <= self.max_dimension
            && x >= -reach
            && y >= -reach
            && x + w <= reach - 1.0
            && y + h <= reach - 1.0;
        if !valid {
            return false;
        }
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.set_pipeline(&self.blit);
        pass.set_bind_group(0, &target.blit_group, &[]);
        pass.draw(0..3, 0..1);
        true
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = length(a);
    if l > 1e-12 { a.map(|v| v / l) } else { [0.0, 0.0, 1.0] }
}

/// A right-handed view matrix (column-major), +y up.
fn look_at(eye: [f32; 3], target: [f32; 3]) -> [f32; 16] {
    let f = normalize(sub(target, eye));
    let s = normalize(cross(f, [0.0, 1.0, 0.0]));
    let u = cross(s, f);
    [s[0], u[0], -f[0], 0.0, s[1], u[1], -f[1], 0.0, s[2], u[2], -f[2], 0.0, -dot(s, eye), -dot(u, eye), dot(f, eye), 1.0]
}

/// A right-handed perspective projection (column-major) onto wgpu's 0..1 depth range.
fn perspective(fov: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov / 2.0).tan();
    let range = near - far;
    [f / aspect, 0.0, 0.0, 0.0, 0.0, f, 0.0, 0.0, 0.0, 0.0, far / range, -1.0, 0.0, 0.0, near * far / range, 0.0]
}

/// `a × b` for column-major 4×4 matrices.
fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for (index, cell) in out.iter_mut().enumerate() {
        let (column, row) = (index / 4, index % 4);
        *cell = (0..4).map(|k| a.get(k * 4 + row).copied().unwrap_or(0.0) * b.get(column * 4 + k).copied().unwrap_or(0.0)).sum();
    }
    out
}

#[cfg(test)]
mod tests;
