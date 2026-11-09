//! Original 300×300 tessellated height-field pyramid, generated for conformance.
//! Triangles cover distinct cells: the large-file fixture does not benchmark pathological
//! coplanar overdraw or an easy tiny mesh padded with comments.
pub fn large() -> Vec<u8> {
    let mut bytes = vec![0u8; 84];
    if let Some(count) = bytes.get_mut(80..84) {
        count.copy_from_slice(&180_000u32.to_le_bytes());
    }
    let point = |x: i32, y: i32| [x as f32 / 256.0, y as f32 / 256.0, (150 - (x - 150).abs() - (y - 150).abs()).max(0) as f32 / 256.0];
    for y in 0..300 {
        for x in 0..300 {
            for triangle in [[point(x, y), point(x + 1, y), point(x + 1, y + 1)], [point(x, y), point(x + 1, y + 1), point(x, y + 1)]] {
                for v in [0.0f32, 0.0, 1.0].into_iter().chain(triangle.into_iter().flatten()) {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                bytes.extend_from_slice(&[0, 0]);
            }
        }
    }
    bytes
}
