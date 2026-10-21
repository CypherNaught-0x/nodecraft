use super::*;

fn apply(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    let v = [p[0], p[1], p[2], 1.0];
    let row = |r: usize| (0..4).map(|k| m[k * 4 + r] * v[k]).sum::<f32>();
    let w = row(3);
    [row(0) / w, row(1) / w, row(2) / w]
}

#[test]
fn the_default_view_keeps_the_whole_model_in_frame_and_in_depth() {
    let (center, radius) = ([1.0, 2.0, 3.0], 2.5);
    let matrix = Orbit::default().view_projection(center, radius, 1.5);
    for dx in [-1.0, 1.0] {
        for dy in [-1.0, 1.0] {
            for dz in [-1.0, 1.0] {
                let s = radius / 3f32.sqrt();
                let [x, y, z] = apply(&matrix, [center[0] + dx * s, center[1] + dy * s, center[2] + dz * s]);
                assert!(x.abs() <= 1.0 && y.abs() <= 1.0, "corner off screen: {x} {y}");
                assert!((0.0..=1.0).contains(&z), "corner clipped by depth: {z}");
            }
        }
    }
    let [x, y, _] = apply(&matrix, center);
    assert!(x.abs() < 1e-4 && y.abs() < 1e-4, "the centre is not centred");
}

#[test]
fn nearer_points_have_smaller_depth() {
    let orbit = Orbit { yaw: 0.0, pitch: 0.0, zoom: 1.0 };
    let matrix = orbit.view_projection([0.0; 3], 1.0, 1.0);
    // Yaw 0 puts the camera on +z looking down -z.
    let near = apply(&matrix, [0.0, 0.0, 0.5])[2];
    let far = apply(&matrix, [0.0, 0.0, -0.5])[2];
    assert!(near < far, "{near} !< {far}");
}

#[test]
fn hostile_numbers_never_make_a_broken_camera() {
    for (radius, aspect) in [(f32::NAN, 1.0), (0.0, f32::INFINITY), (-3.0, 0.0), (f32::INFINITY, f32::NAN)] {
        let matrix = Orbit::default().view_projection([0.0; 3], radius, aspect);
        assert!(matrix.iter().all(|v| v.is_finite()), "{radius} {aspect}: {matrix:?}");
    }
    let orbit = Orbit::default().turned(f32::NAN, f32::INFINITY).zoomed(f32::NAN).zoomed(-2.0);
    assert_eq!(orbit.zoom, 1.0);
    assert!(orbit.yaw.is_finite() && orbit.pitch.is_finite());
    let tilted = Orbit::default().turned(100.0, 100.0);
    assert!(tilted.pitch <= Orbit::MAX_PITCH && (0.0..std::f32::consts::TAU).contains(&tilted.yaw));
    assert_eq!(Orbit::default().zoomed(1e9).zoom, Orbit::MAX_ZOOM);
    assert_eq!(Orbit::default().zoomed(1e-9).zoom, Orbit::MIN_ZOOM);
}

#[test]
fn the_light_follows_the_camera() {
    let a = Orbit::default().light();
    let b = Orbit::default().turned(1.0, 0.0).light();
    assert!((length(a) - 1.0).abs() < 1e-5 && a != b);
}

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

const QUAD: [[f32; 3]; 4] = [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]];

fn quad<'a>(colors: &'a [[f32; 4]], edges: Option<&'a [u32]>) -> MeshData<'a> {
    MeshData { positions: &QUAD, normals: &[[0.0, 0.0, 1.0]; 4], colors, indices: &[0, 1, 2, 0, 2, 3], edges }
}

#[test]
fn uploads_refuse_inconsistent_models() {
    let Some((device, queue)) = device() else { return };
    let red = [[1.0, 0.0, 0.0, 1.0]; 4];
    assert!(GpuMesh::upload(&device, &queue, quad(&red, Some(&[0, 1, 1, 2]))).is_ok_and(|m| m.has_edges() && m.bytes() > 0));
    assert_eq!(GpuMesh::upload(&device, &queue, quad(&red[..3], None)).err(), Some(MeshUploadError::Invalid));
    assert_eq!(GpuMesh::upload(&device, &queue, quad(&red, Some(&[0, 9]))).err(), Some(MeshUploadError::Invalid));
    assert_eq!(GpuMesh::upload(&device, &queue, quad(&red, Some(&[0]))).err(), Some(MeshUploadError::Invalid));
    let bad = MeshData { indices: &[0, 1, 4], ..quad(&red, None) };
    assert_eq!(GpuMesh::upload(&device, &queue, bad).err(), Some(MeshUploadError::Invalid));
    let empty = MeshData { indices: &[], ..quad(&red, None) };
    assert_eq!(GpuMesh::upload(&device, &queue, empty).err(), Some(MeshUploadError::Invalid));
}

#[test]
fn targets_keep_their_shape_within_the_pixel_budget() {
    let Some((device, _queue)) = device() else { return };
    let renderer = MeshRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb, 1);
    assert_eq!(renderer.clamp_size([640, 360]), [640, 360], "an ordinary card is drawn at its own size");
    assert_eq!(renderer.clamp_size([0, 0]), [1, 1]);
    for size in [[8192, 1024], [4096, 4096], [u32::MAX, 3], [3, u32::MAX]] {
        let [w, h] = renderer.clamp_size(size);
        assert!(u64::from(w) * u64::from(h) <= MAX_TARGET_PIXELS && w <= MAX_TARGET_SIDE && h <= MAX_TARGET_SIDE, "{size:?} → {w}×{h}");
        let (want, got) = (f64::from(size[0]) / f64::from(size[1]), f64::from(w) / f64::from(h));
        assert!(w == 1 || h == 1 || (want / got - 1.0).abs() < 0.01, "{size:?} → {w}×{h} keeps its shape");
    }
    let target = renderer.target(&device, [8192, 1024]).unwrap();
    assert_eq!(target.bytes(), u64::from(target.size()[0]) * u64::from(target.size()[1]) * TARGET_PIXEL_BYTES);
}

/// Renders the quad and reads the centre and a corner pixel back.
fn render(device: &wgpu::Device, queue: &wgpu::Queue, mesh: &GpuMesh, style: Style) -> ([u8; 4], [u8; 4]) {
    let renderer = MeshRenderer::new(device, wgpu::TextureFormat::Rgba8UnormSrgb, 1);
    let target = renderer.target(device, [64, 64]).unwrap();
    let view = View {
        orbit: Orbit { yaw: 0.0, pitch: 0.0, zoom: 1.0 },
        style,
        center: [0.0; 3],
        radius: 2f32.sqrt(),
        background: [0.0, 0.0, 1.0, 1.0],
        line: [0.0, 1.0, 0.0, 1.0],
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    renderer.render(queue, &mut encoder, &target, mesh, &view);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256 * 64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        target.image.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(64) } },
        wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().map(|v| v.to_vec()).unwrap_or_default();
    let pixel = |x: usize, y: usize| {
        let i = y * 256 + x * 4;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };
    (pixel(32, 32), pixel(0, 0))
}

#[test]
fn a_model_renders_lit_unlit_and_as_lines_over_the_background() {
    let Some((device, queue)) = device() else { return };
    let red = [[1.0, 0.0, 0.0, 1.0]; 4];
    let edges = [0, 1, 1, 2, 2, 3, 3, 0, 1, 3];
    let Ok(mesh) = GpuMesh::upload(&device, &queue, quad(&red, Some(&edges))) else { panic!("upload") };
    let (centre, corner) = render(&device, &queue, &mesh, Style::Unlit);
    assert_eq!(centre, [255, 0, 0, 255]);
    assert_eq!(corner, [0, 0, 255, 255], "the background shows outside the model");
    let (shaded, _) = render(&device, &queue, &mesh, Style::Shaded);
    assert!(shaded[0] > 60 && shaded[0] < 255 && shaded[1] == 0, "lit, but not at full strength: {shaded:?}");
    // The centre pixel sits on the diagonal edge.
    let (line, corner) = render(&device, &queue, &mesh, Style::Wireframe);
    assert!(line[1] > 100 && line[0] == 0, "a green line through the middle: {line:?}");
    assert_eq!(corner, [0, 0, 255, 255]);
}
