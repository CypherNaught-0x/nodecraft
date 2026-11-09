//! Bounded, self-contained single-USDA-layer USDZ triangle/RGBA profile (#1019).
mod container;
mod usda;

use super::{Mesh, MeshLimits};
use crate::MediaError;
use std::fmt::{self, Write};

pub(crate) fn recognizes(bytes: &[u8]) -> bool {
    container::recognizes(bytes)
}
fn invalid(message: &'static str) -> MediaError {
    MediaError::Decode(message.into())
}
#[cfg(test)]
thread_local! {static ALLOCATIONS:std::cell::Cell<[usize;2]>=const{std::cell::Cell::new([0;2])};}
#[cfg(test)]
pub(super) fn reset_allocations() {
    ALLOCATIONS.with(|c| c.set([0; 2]));
}
#[cfg(test)]
pub(super) fn allocations() -> [usize; 2] {
    ALLOCATIONS.with(std::cell::Cell::get)
}
#[cfg(test)]
pub(super) fn package_layer(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    container::encode(bytes.len(), usize::MAX, |out| {
        out.extend_from_slice(bytes);
        Ok(())
    })
}
#[cfg(test)]
pub(super) fn layer(bytes: &[u8]) -> Result<&[u8], MediaError> {
    container::layer(bytes)
}
#[cfg(test)]
fn allocated(kind: usize) {
    ALLOCATIONS.with(|c| {
        let mut v = c.get();
        if let Some(n) = v.get_mut(kind) {
            *n += 1;
        }
        c.set(v);
    });
}

pub(super) fn decode(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    let hard = MeshLimits::default();
    let limits = MeshLimits {
        max_input_bytes: limits.max_input_bytes.min(hard.max_input_bytes),
        max_vertices: limits.max_vertices.min(hard.max_vertices),
        max_triangles: limits.max_triangles.min(hard.max_triangles),
        ..*limits
    };
    if bytes.len() > limits.max_input_bytes {
        return Err(MediaError::LimitExceeded("USDZ encoded input exceeds its byte limit".into()));
    }
    let text = usda::text(container::layer(bytes)?)?;
    // Pass one checks the complete container/grammar/counts/numeric attributes and index range,
    // plus derived bounds, with borrowed tokens and constant metadata only.
    let counts = usda::parse(text, &limits, None)?;
    let indices = counts.triangles.checked_mul(3).ok_or_else(|| invalid("USDZ index count overflow"))?;
    let allocation = |_| MediaError::LimitExceeded("USDZ indexed geometry allocation refused".into());
    #[cfg(test)]
    allocated(0);
    let mut mesh = Mesh::default();
    mesh.positions.try_reserve_exact(counts.vertices).map_err(allocation)?;
    mesh.normals.try_reserve_exact(counts.vertices).map_err(allocation)?;
    mesh.colors.try_reserve_exact(counts.vertices).map_err(allocation)?;
    mesh.indices.try_reserve_exact(indices).map_err(allocation)?;
    mesh.positions.resize(counts.vertices, [0.; 3]);
    mesh.normals.resize(counts.vertices, [0.; 3]);
    mesh.colors.resize(counts.vertices, [0.; 4]);
    mesh.indices.resize(indices, 0);
    if usda::parse(text, &limits, Some(&mut mesh))? != counts {
        return Err(invalid("USDZ geometry extent changed"));
    }
    // Indexed area validation needs the admitted position/index buffers. They are not adopted
    // until every face passes. Peak geometry is exactly40V+12T bytes (208MB at default caps).
    usda::validate_geometry(&mesh)?;
    Ok(mesh)
}
struct Counter {
    len: usize,
    cap: usize,
}
impl Write for Counter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.len = self.len.checked_add(s.len()).filter(|n| *n <= self.cap).ok_or(fmt::Error)?;
        Ok(())
    }
}
struct Bytes<'a> {
    out: &'a mut Vec<u8>,
    end: usize,
}
impl Write for Bytes<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.out.len().checked_add(s.len()).filter(|n| *n <= self.end).ok_or(fmt::Error)?;
        self.out.extend_from_slice(s.as_bytes());
        Ok(())
    }
}
/// Writes one aligned, unencrypted STORE USDA layer, with indexed triangles, float32 positions,
/// normalized vertex normals and linear unassociated RGBA. The layer declares Y-up/unit meters
/// as this profile's unscaled coordinate convention. No materials/resources or transparency
/// rendering are added. All geometry and formatted ZIP-inclusive size pass before allocation.
pub fn encode_usdz(mesh: &Mesh, max_output_bytes: usize) -> Result<Vec<u8>, MediaError> {
    usda::validate_geometry(mesh)?;
    let hard = MeshLimits::default();
    if mesh.vertex_count() > hard.max_vertices || mesh.triangle_count() > hard.max_triangles {
        return Err(MediaError::LimitExceeded("USDZ output exceeds default geometry limits".into()));
    }
    let cap = max_output_bytes.min(hard.max_input_bytes);
    let body_cap =
        cap.checked_sub(container::OVERHEAD).ok_or_else(|| MediaError::LimitExceeded("USDZ complete package exceeds its output byte limit".into()))?;
    let mut counter = Counter { len: 0, cap: body_cap };
    usda::render(mesh, &mut counter)?;
    #[cfg(test)]
    allocated(1);
    container::encode(counter.len, cap, |out| {
        let end = out.len().checked_add(counter.len).ok_or_else(|| invalid("USDZ output extent overflow"))?;
        usda::render(mesh, &mut Bytes { out, end })
    })
}
