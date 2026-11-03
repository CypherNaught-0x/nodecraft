//! Original bounded binary STL profile (#569). Geometry only; no colour extensions or units.
use super::{Mesh, MeshLimits};
use crate::MediaError;

const HEADER: usize = 84;
const FACET: usize = 50;

fn invalid(message: &'static str) -> MediaError {
    MediaError::Decode(message.into())
}
fn extent(count: usize) -> Option<usize> {
    count.checked_mul(FACET)?.checked_add(HEADER)
}
fn count(bytes: &[u8]) -> Option<usize> {
    let word = bytes.get(80..84)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(word)).ok()
}

/// A bounded recognition check, not validation. Binary headers have no required magic.
pub(crate) fn recognizes(bytes: &[u8]) -> bool {
    count(bytes).is_some_and(|n| n > 0 && extent(n) == Some(bytes.len())) || ascii_header(bytes)
}

// Recognize the unsupported ASCII variant too, so importing it reports a useful error rather
// than creating a text card. This only examines the same 1 KiB window as general sniffing.
fn ascii_header(bytes: &[u8]) -> bool {
    let Ok(head) = std::str::from_utf8(bytes.get(..bytes.len().min(1024)).unwrap_or_default()) else { return false };
    let mut lines = head.trim_start().lines().map(str::trim);
    if !lines.next().is_some_and(|s| s == "solid" || s.starts_with("solid ")) {
        return false;
    }
    let (mut normal, mut outer, mut vertex) = (false, false, false);
    for line in lines {
        normal |= line.starts_with("facet normal ");
        outer |= line == "outer loop";
        vertex |= line.starts_with("vertex ");
    }
    normal && outer && vertex
}

/// Facet normals follow winding. f64 intermediates keep finite f32 coordinates from
/// overflowing or underflowing during the cross product; zero-area facets are refused.
fn normal(vertices: [[f32; 3]; 3]) -> Result<[f32; 3], MediaError> {
    let [a, b, c] = vertices.map(|p| p.map(f64::from));
    let [ax, ay, az] = a;
    let [bx, by, bz] = b;
    let [cx, cy, cz] = c;
    let (ux, uy, uz) = (bx - ax, by - ay, bz - az);
    let (vx, vy, vz) = (cx - ax, cy - ay, cz - az);
    let cross = [uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx];
    let length = cross.iter().map(|n| n * n).sum::<f64>().sqrt();
    if !length.is_finite() || length == 0.0 {
        return Err(invalid("binary STL cannot represent a non-finite or zero-area facet"));
    }
    Ok(cross.map(|n| (n / length) as f32))
}

struct Triangle {
    vertices: [[f32; 3]; 3],
    normal: [f32; 3],
}

fn facet(bytes: &[u8; FACET]) -> Result<Triangle, MediaError> {
    if !bytes.get(48..50).is_some_and(|attribute| attribute == [0, 0]) {
        return Err(invalid("binary STL facet attributes and colour extensions are unsupported"));
    }
    let words = bytes.get(..48).ok_or_else(|| invalid("truncated binary STL facet"))?.as_chunks::<4>().0;
    let mut values = [[0.0f32; 3]; 4];
    for (out, word) in values.iter_mut().flatten().zip(words) {
        *out = f32::from_le_bytes(*word);
        if !out.is_finite() {
            return Err(invalid("binary STL contains a non-finite normal or vertex"));
        }
    }
    let [_, a, b, c] = values;
    let vertices = [a, b, c];
    Ok(Triangle { vertices, normal: normal(vertices)? })
}

pub(super) fn decode(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    let n = count(bytes).ok_or_else(|| invalid("binary STL needs an 80-byte header and a facet count"))?;
    if extent(n) != Some(bytes.len()) {
        return Err(if ascii_header(bytes) {
            invalid("ASCII STL is unsupported; save the model as binary STL")
        } else {
            invalid("binary STL facet count does not match the complete file length")
        });
    }
    if n == 0 {
        return Err(invalid("binary STL has no triangles to show"));
    }
    let vertices = n.checked_mul(3).ok_or_else(|| invalid("binary STL vertex count overflow"))?;
    if n > limits.max_triangles || vertices > limits.max_vertices || u32::try_from(vertices).is_err() {
        return Err(MediaError::LimitExceeded("binary STL exceeds the decoded vertex or triangle limits".into()));
    }
    let body = bytes.get(HEADER..).ok_or_else(|| invalid("truncated binary STL body"))?.as_chunks::<FACET>().0;
    // Validate every facet before any geometry allocation, including an invalid final facet.
    for bytes in body {
        facet(bytes)?;
    }
    let mut mesh = Mesh::default();
    let allocation = |_| MediaError::LimitExceeded("binary STL geometry allocation refused".into());
    mesh.positions.try_reserve_exact(vertices).map_err(allocation)?;
    mesh.normals.try_reserve_exact(vertices).map_err(allocation)?;
    mesh.colors.try_reserve_exact(vertices).map_err(allocation)?;
    mesh.indices.try_reserve_exact(vertices).map_err(allocation)?;
    for bytes in body {
        let triangle = facet(bytes)?;
        for position in triangle.vertices {
            let index = u32::try_from(mesh.positions.len()).map_err(|_| invalid("binary STL index overflow"))?;
            mesh.positions.push(position);
            mesh.normals.push(triangle.normal);
            mesh.colors.push([1.0; 4]);
            mesh.indices.push(index);
        }
    }
    Ok(mesh)
}

fn triangle(mesh: &Mesh, indices: [u32; 3]) -> Result<Triangle, MediaError> {
    let vertex =
        |index| usize::try_from(index).ok().and_then(|i| mesh.positions.get(i)).copied().ok_or_else(|| invalid("STL triangle refers to a missing vertex"));
    let [a, b, c] = indices;
    let vertices = [vertex(a)?, vertex(b)?, vertex(c)?];
    Ok(Triangle { vertices, normal: normal(vertices)? })
}

/// Binary STL with one flat normal and three vertices per facet, and zero attribute bytes.
/// Preserves coordinates and winding without scaling; discards colours/materials/UVs. Neither
/// this writer nor the reader certifies a watertight or manufacturing-ready surface.
///
/// The complete output size and all facets are admitted before allocating the output. Generated
/// files also fit the default reader's byte/vertex/triangle limits; STL has three decoded
/// vertices per triangle. A caller may lower the encoded output allowance further.
pub fn encode_stl(mesh: &Mesh, max_output_bytes: usize) -> Result<Vec<u8>, MediaError> {
    mesh.validate()?;
    let n = mesh.triangle_count();
    if n == 0 {
        return Err(MediaError::InvalidParameter("an empty mesh cannot be written as binary STL"));
    }
    let count = u32::try_from(n).map_err(|_| MediaError::LimitExceeded("too many binary STL facets".into()))?;
    let profile = MeshLimits::default();
    if n > profile.max_triangles || !n.checked_mul(3).is_some_and(|vertices| vertices <= profile.max_vertices) {
        return Err(MediaError::LimitExceeded("binary STL output exceeds the decoded geometry limits".into()));
    }
    let length = extent(n)
        .filter(|n| *n <= max_output_bytes.min(profile.max_input_bytes))
        .ok_or_else(|| MediaError::LimitExceeded("binary STL output exceeds its byte limit".into()))?;
    for indices in mesh.indices.as_chunks::<3>().0 {
        triangle(mesh, *indices)?;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(length).map_err(|_| MediaError::LimitExceeded("binary STL output allocation refused".into()))?;
    out.extend_from_slice(b"NodeCraft binary STL");
    out.resize(80, 0);
    out.extend_from_slice(&count.to_le_bytes());
    for indices in mesh.indices.as_chunks::<3>().0 {
        let triangle = triangle(mesh, *indices)?;
        for value in triangle.normal.iter().chain(triangle.vertices.iter().flatten()) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    if out.len() != length {
        return Err(invalid("binary STL output extent changed"));
    }
    Ok(out)
}
