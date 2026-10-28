//! Original ASCII PLY1.0 triangle/RGBA profile (#1011). No external resources or materials.
use super::{Mesh, MeshLimits};
use crate::MediaError;
use std::fmt::{self, Write};
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_LINES: usize = 64;
const MAX_LINE_BYTES: usize = 1024;
const MAX_TOKEN_BYTES: usize = 64;
#[cfg(test)]
thread_local! {static ALLOCATIONS:std::cell::Cell<[usize;2]>=const{std::cell::Cell::new([0;2])};}
#[cfg(test)]
pub(super) fn allocations() -> [usize; 2] {
    ALLOCATIONS.with(std::cell::Cell::get)
}
#[cfg(test)]
pub(super) fn reset_allocations() {
    ALLOCATIONS.with(|v| v.set([0; 2]));
}
#[cfg(test)]
fn note_allocation(kind: usize) {
    ALLOCATIONS.with(|v| {
        let mut n = v.get();
        if let Some(x) = n.get_mut(kind) {
            *x += 1;
        }
        v.set(n);
    });
}
fn invalid(s: &'static str) -> MediaError {
    MediaError::Decode(s.into())
}
fn output_limit() -> MediaError {
    MediaError::LimitExceeded("PLY formatted output exceeds its byte limit".into())
}
/// Recognition is magic only. A malformed/unsupported PLY must reach the named format error.
pub(crate) fn recognizes(b: &[u8]) -> bool {
    b.starts_with(b"ply\n") || b.starts_with(b"ply\r")
}
#[derive(Clone, Copy)]
struct Lines<'a> {
    text: &'a str,
    at: usize,
}
impl<'a> Lines<'a> {
    fn next(&mut self) -> Result<Option<&'a str>, MediaError> {
        if self.at == self.text.len() {
            return Ok(None);
        }
        let window = self
            .text
            .as_bytes()
            .get(self.at..self.at.saturating_add(MAX_LINE_BYTES + 2).min(self.text.len()))
            .ok_or_else(|| invalid("invalid PLY line extent"))?;
        let n = window.iter().position(|b| matches!(b, b'\r' | b'\n')).unwrap_or(window.len());
        if n > MAX_LINE_BYTES || n == window.len() && self.at.saturating_add(n) != self.text.len() {
            return Err(invalid("PLY line exceeds 1024 bytes"));
        }
        let end = self.at.checked_add(n).ok_or_else(|| invalid("PLY line extent overflow"))?;
        let line = self.text.get(self.at..end).ok_or_else(|| invalid("invalid PLY ASCII line"))?;
        if !line.bytes().all(|b| b == b'\t' || (32..=126).contains(&b)) {
            return Err(invalid("PLY requires printable ASCII with ordinary line endings"));
        }
        self.at = end;
        if self.text.as_bytes().get(self.at) == Some(&b'\r') {
            self.at = self.at.saturating_add(1);
            if self.text.as_bytes().get(self.at) == Some(&b'\n') {
                self.at = self.at.saturating_add(1);
            }
        } else if self.text.as_bytes().get(self.at) == Some(&b'\n') {
            self.at = self.at.saturating_add(1);
        }
        Ok(Some(line))
    }
}
fn words<const N: usize>(line: &str) -> Result<[&str; N], MediaError> {
    let mut out = [""; N];
    let mut count = 0usize;
    for (i, token) in line.split_ascii_whitespace().enumerate() {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(invalid("PLY token exceeds 64 bytes"));
        }
        let target = out.get_mut(i).ok_or_else(|| invalid("PLY row has too many values"))?;
        *target = token;
        count += 1;
    }
    if count != N {
        return Err(invalid("PLY row is missing required values"));
    }
    Ok(out)
}
fn header<'a>(lines: &mut Lines<'a>, count: &mut usize) -> Result<&'a str, MediaError> {
    loop {
        let line = lines.next()?.ok_or_else(|| invalid("truncated PLY header"))?;
        *count = count.saturating_add(1);
        if *count > MAX_HEADER_LINES || lines.at > MAX_HEADER_BYTES {
            return Err(invalid("PLY header exceeds 64 lines or 16 KiB"));
        }
        if line == "comment" || line.starts_with("comment ") || line.starts_with("comment\t") {
            continue;
        }
        return Ok(line);
    }
}
fn count(line: &str, name: &str, cap: usize) -> Result<usize, MediaError> {
    let [element, kind, number] = words::<3>(line)?;
    if element != "element" || kind != name {
        return Err(invalid("PLY supports vertex then face elements only"));
    }
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("PLY element count must be a positive decimal integer"));
    }
    number.parse::<usize>().ok().filter(|n| *n > 0 && *n <= cap).ok_or_else(|| MediaError::LimitExceeded("PLY element count exceeds its geometry limit".into()))
}
fn property(line: &str, kind: &str, name: &str) -> Result<(), MediaError> {
    let [keyword, ty, field] = words::<3>(line)?;
    let typed = ty == kind || matches!((kind, ty), ("float", "float32") | ("uchar", "uint8"));
    if keyword != "property" || !typed || field != name {
        return Err(invalid("PLY requires ordered position, normal and RGBA vertex properties"));
    }
    Ok(())
}
fn scalar(token: &str) -> Result<f32, MediaError> {
    let value = token.parse::<f32>().map_err(|_| invalid("PLY vertex values must be finite float32 numbers"))?;
    if !value.is_finite() || value == 0.0 && token.split(['e', 'E']).next().is_some_and(|mantissa| mantissa.bytes().any(|b| matches!(b, b'1'..=b'9'))) {
        return Err(invalid("PLY vertex value is outside finite float32 range"));
    }
    Ok(value)
}
fn unit(n: [f32; 3]) -> Result<[f32; 3], MediaError> {
    let values = n.map(f64::from);
    let length = values.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !length.is_finite() || length == 0.0 {
        return Err(invalid("PLY normals must be finite and nonzero"));
    }
    Ok(values.map(|v| (v / length) as f32))
}
fn channel(token: &str) -> Result<u8, MediaError> {
    if !token.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("PLY RGBA channels must be integers from 0 through 255"));
    }
    token.parse().map_err(|_| invalid("PLY RGBA channels must be integers from 0 through 255"))
}
fn linear(c: u8) -> f32 {
    let v = f64::from(c) / 255.0;
    if v <= 0.04045 { (v / 12.92) as f32 } else { ((v + 0.055) / 1.055).powf(2.4) as f32 }
}
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 4],
}
fn vertex(line: &str) -> Result<Vertex, MediaError> {
    let [x, y, z, nx, ny, nz, r, g, b, a] = words::<10>(line)?;
    Ok(Vertex {
        position: [scalar(x)?, scalar(y)?, scalar(z)?],
        normal: unit([scalar(nx)?, scalar(ny)?, scalar(nz)?])?,
        color: [linear(channel(r)?), linear(channel(g)?), linear(channel(b)?), f32::from(channel(a)?) / 255.0],
    })
}
fn face(line: &str, vertices: usize) -> Result<[u32; 3], MediaError> {
    let [n, a, b, c] = words::<4>(line)?;
    if n != "3" {
        return Err(invalid("PLY supports triangle faces with exactly three indices"));
    }
    let mut out = [0u32; 3];
    for (out, token) in out.iter_mut().zip([a, b, c]) {
        if !token.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid("PLY face indices must be nonnegative decimal integers"));
        }
        *out = token
            .parse::<u32>()
            .ok()
            .filter(|n| usize::try_from(*n).is_ok_and(|n| n < vertices))
            .ok_or_else(|| invalid("PLY face index refers to a missing vertex"))?;
    }
    Ok(out)
}
fn body(mut lines: Lines<'_>, vertices: usize, faces: usize, mut mesh: Option<&mut Mesh>) -> Result<(), MediaError> {
    for _ in 0..vertices {
        let v = vertex(lines.next()?.ok_or_else(|| invalid("truncated PLY vertex list"))?)?;
        if let Some(mesh) = mesh.as_deref_mut() {
            mesh.positions.push(v.position);
            mesh.normals.push(v.normal);
            mesh.colors.push(v.color);
        }
    }
    for _ in 0..faces {
        let indices = face(lines.next()?.ok_or_else(|| invalid("truncated PLY face list"))?, vertices)?;
        if let Some(mesh) = mesh.as_deref_mut() {
            mesh.indices.extend(indices);
        }
    }
    if lines.next()?.is_some() {
        return Err(invalid("PLY has trailing data after the declared triangle list"));
    }
    Ok(())
}
pub(super) fn decode(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    let hard = MeshLimits::default();
    if bytes.len() > limits.max_input_bytes.min(hard.max_input_bytes) {
        return Err(MediaError::LimitExceeded("PLY encoded input exceeds its byte limit".into()));
    }
    if !bytes.is_ascii() {
        return Err(invalid("this PLY profile requires ASCII"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("this PLY profile requires ASCII"))?;
    let mut lines = Lines { text, at: 0 };
    let mut n = 0usize;
    if !recognizes(bytes) || header(&mut lines, &mut n)? != "ply" {
        return Err(invalid("PLY must begin with its ASCII magic line"));
    }
    if words::<3>(header(&mut lines, &mut n)?)? != ["format", "ascii", "1.0"] {
        return Err(invalid("only PLY ASCII 1.0 is supported"));
    }
    let vertices = count(header(&mut lines, &mut n)?, "vertex", limits.max_vertices.min(hard.max_vertices).min(i32::MAX as usize))?;
    for name in ["x", "y", "z", "nx", "ny", "nz"] {
        property(header(&mut lines, &mut n)?, "float", name)?;
    }
    for name in ["red", "green", "blue", "alpha"] {
        property(header(&mut lines, &mut n)?, "uchar", name)?;
    }
    let faces = count(header(&mut lines, &mut n)?, "face", limits.max_triangles.min(hard.max_triangles))?;
    let [p, list, c, i, name] = words::<5>(header(&mut lines, &mut n)?)?;
    if p != "property"
        || list != "list"
        || !matches!(c, "uchar" | "uint8")
        || !matches!(i, "int" | "int32")
        || !matches!(name, "vertex_index" | "vertex_indices")
    {
        return Err(invalid("PLY faces require one uchar/int vertex-index list"));
    }
    if words::<1>(header(&mut lines, &mut n)?)? != ["end_header"] {
        return Err(invalid("unsupported PLY header element or property"));
    }
    // No geometry vector may reserve until the entire immutable body has passed validation.
    body(lines, vertices, faces, None)?;
    let indices = faces.checked_mul(3).ok_or_else(|| invalid("PLY index count overflow"))?;
    #[cfg(test)]
    note_allocation(0);
    let mut mesh = Mesh::default();
    let allocation = |_| MediaError::LimitExceeded("PLY geometry allocation refused".into());
    mesh.positions.try_reserve_exact(vertices).map_err(allocation)?;
    mesh.normals.try_reserve_exact(vertices).map_err(allocation)?;
    mesh.colors.try_reserve_exact(vertices).map_err(allocation)?;
    mesh.indices.try_reserve_exact(indices).map_err(allocation)?;
    body(lines, vertices, faces, Some(&mut mesh))?;
    Ok(mesh)
}
fn quantize(color: [f32; 4]) -> Result<[u8; 4], MediaError> {
    let mut out = [0u8; 4];
    for (i, (out, value)) in out.iter_mut().zip(color).enumerate() {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(invalid("PLY color channels must be finite within 0 through 1"));
        }
        let value = f64::from(value);
        let encoded = if i == 3 {
            value
        } else if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        // Finite unit channels imply a finite0..255 result before this bounded cast.
        *out = (encoded * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Ok(out)
}
fn render(mesh: &Mesh, out: &mut dyn Write) -> Result<(), MediaError> {
    write!(out,"ply\nformat ascii 1.0\nelement vertex {}\nproperty float x\nproperty float y\nproperty float z\nproperty float nx\nproperty float ny\nproperty float nz\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nproperty uchar alpha\nelement face {}\nproperty list uchar int vertex_indices\nend_header\n",mesh.vertex_count(),mesh.triangle_count()).map_err(|_|output_limit())?;
    for ((p, n), c) in mesh.positions.iter().zip(&mesh.normals).zip(&mesh.colors) {
        let [x, y, z] = *p;
        let [nx, ny, nz] = unit(*n)?;
        let [r, g, b, a] = quantize(*c)?;
        writeln!(out, "{x} {y} {z} {nx} {ny} {nz} {r} {g} {b} {a}").map_err(|_| output_limit())?;
    }
    for [a, b, c] in mesh.indices.as_chunks::<3>().0 {
        writeln!(out, "3 {a} {b} {c}").map_err(|_| output_limit())?;
    }
    Ok(())
}
struct Counter {
    bytes: usize,
    cap: usize,
}
impl Write for Counter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.bytes = self.bytes.checked_add(s.len()).filter(|n| *n <= self.cap).ok_or(fmt::Error)?;
        Ok(())
    }
}
/// One ASCII1.0 triangle object with float32 positions/unit normals and uint8 RGBA.
/// RGB encodes linear channels as rounded sRGB8; alpha is rounded linear coverage8.
/// All channel data must be finite in0..1; normals must be finite/nonzero. The reader
/// normalizes normals. No textures/materials/units or transparency-rendering claim is made.
/// Counts, every attribute and the full formatted extent pass before output allocation.
pub fn encode_ply(mesh: &Mesh, max_output_bytes: usize) -> Result<Vec<u8>, MediaError> {
    mesh.validate()?;
    let hard = MeshLimits::default();
    if mesh.vertex_count() == 0 || mesh.triangle_count() == 0 {
        return Err(invalid("an empty mesh cannot be written as PLY"));
    }
    if mesh.vertex_count() > hard.max_vertices || mesh.triangle_count() > hard.max_triangles {
        return Err(MediaError::LimitExceeded("PLY output exceeds default geometry limits".into()));
    }
    let mut counter = Counter { bytes: 0, cap: max_output_bytes.min(hard.max_input_bytes) };
    render(mesh, &mut counter)?;
    #[cfg(test)]
    note_allocation(1);
    let mut text = String::new();
    text.try_reserve_exact(counter.bytes).map_err(|_| MediaError::LimitExceeded("PLY output allocation refused".into()))?;
    render(mesh, &mut text)?;
    if text.len() != counter.bytes {
        return Err(invalid("PLY formatted output extent changed"));
    }
    Ok(text.into_bytes())
}
