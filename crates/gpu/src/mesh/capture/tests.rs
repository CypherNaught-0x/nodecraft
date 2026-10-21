use super::*;
use crate::mesh::{Orbit, Style};

const QUAD: [[f32; 3]; 4] = [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]];
const RED: [[f32; 4]; 4] = [[1.0, 0.0, 0.0, 1.0]; 4];

fn quad() -> MeshData<'static> {
    MeshData { positions: &QUAD, normals: &[[0.0, 0.0, 1.0]; 4], colors: &RED, indices: &[0, 1, 2, 0, 2, 3], edges: None }
}

fn view(yaw: f32, background: [f32; 4]) -> View {
    View { orbit: Orbit { yaw, pitch: 0.0, zoom: 1.0 }, style: Style::Unlit, center: [0.0; 3], radius: 2f32.sqrt(), background, line: [1.0; 4] }
}

fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    [0, 1, 2, 3].map(|i| pixels[at + i])
}

#[test]
fn a_capture_reads_back_the_model_over_its_background() {
    let Ok(capture) = MeshCapture::request() else { return };
    let mesh = capture.upload(quad()).unwrap();
    // An odd width checks the row padding is stripped.
    let target = capture.target([37, 20]).unwrap();
    let pixels = capture.render(&mesh, &target, &view(0.0, [0.0, 0.0, 1.0, 1.0]), TIMEOUT).unwrap();
    assert_eq!(pixels.len(), 37 * 20 * 4);
    assert_eq!(pixel(&pixels, 37, 18, 10), [255, 0, 0, 255], "the model faces the camera at the centre");
    assert_eq!(pixel(&pixels, 37, 0, 0), [0, 0, 255, 255], "the corner shows the background");
}

#[test]
fn a_transparent_background_stays_transparent_and_views_repeat() {
    let Ok(capture) = MeshCapture::request() else { return };
    let mesh = capture.upload(quad()).unwrap();
    let target = capture.target([32, 32]).unwrap();
    let first = capture.render(&mesh, &target, &view(0.3, [0.0; 4]), TIMEOUT).unwrap();
    assert_eq!(pixel(&first, 32, 0, 0)[3], 0);
    assert_eq!(pixel(&first, 32, 16, 16), [255, 0, 0, 255]);
    // Edge pixels keep the model's colour rather than darkening towards black.
    for p in first.as_chunks::<4>().0.iter().filter(|p| p[3] > 0 && p[3] < 255) {
        assert!(p[0] > 200 && p[1] < 40 && p[2] < 40, "edge pixel {p:?}");
    }
    let other = capture.render(&mesh, &target, &view(1.2, [0.0; 4]), TIMEOUT).unwrap();
    assert_ne!(first, other);
    assert_eq!(capture.render(&mesh, &target, &view(0.3, [0.0; 4]), TIMEOUT).unwrap(), first, "the same view draws the same image");
}

#[test]
fn a_translucent_background_reads_back_as_its_own_colour() {
    let Ok(capture) = MeshCapture::request() else { return };
    let mesh = capture.upload(quad()).unwrap();
    let target = capture.target([32, 32]).unwrap();
    // Linear 0.5 grey at half alpha, premultiplied as `render` takes it: sRGB 188, alpha 128.
    let alpha = 128.0 / 255.0;
    let pixels = capture.render(&mesh, &target, &view(0.0, [0.5 * alpha, 0.5 * alpha, 0.5 * alpha, alpha]), TIMEOUT).unwrap();
    let corner = pixel(&pixels, 32, 0, 0);
    assert!(corner[..3].iter().all(|&v| v.abs_diff(188) <= 2) && corner[3] == 128, "{corner:?}");
    // The model's edges blend with the background, never brighter than either.
    for p in pixels.as_chunks::<4>().0 {
        assert!(p[0] >= 186 && p[1] <= 190 && p[2] <= 190, "edge pixel {p:?}");
    }
}

#[test]
fn unpremultiply_leaves_opaque_and_empty_pixels_alone() {
    let mut pixels = vec![10, 20, 30, 255, 0, 0, 0, 0, 0, 0, 0, 255];
    unpremultiply(&mut pixels);
    assert_eq!(pixels, [10, 20, 30, 255, 0, 0, 0, 0, 0, 0, 0, 255]);
    // Half coverage of linear white over transparent black: linear 0.5, sRGB 188.
    let mut edge = vec![188, 188, 188, 128];
    unpremultiply(&mut edge);
    assert!(edge[..3].iter().all(|&v| v >= 253), "{edge:?}");
    // A short buffer neither panics nor changes.
    let mut short = vec![1, 2, 3];
    unpremultiply(&mut short);
    assert_eq!(short, [1, 2, 3]);
}
