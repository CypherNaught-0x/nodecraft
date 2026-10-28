//! 3D meshes (#111): reading glTF 2.0 (binary `.glb`, or `.gltf` JSON whose buffers are embedded)
//! and Wavefront OBJ into one flat, triangulated [`Mesh`], and writing a mesh back out as GLB or
//! OBJ, geometry-only binary STL or the bounded ASCII PLY/RGBA profile.
//!
//! Every reader takes untrusted bytes. Counts, offsets and strides come from the file, so each is
//! checked against [`MeshLimits`] and the real data before anything is allocated; node hierarchies
//! are walked with a depth and visit budget (a hostile file can make them cyclic). A glTF file that
//! points at an external file is refused: NodeCraft never resolves paths a file names.
//!
//! The mesh is what the viewer draws, not a full scene: positions and normals in one model space,
//! a linear RGBA colour per vertex (the material's base colour, times vertex colours, times its
//! base-colour texture sampled at that vertex) and triangle indices. Fine texture detail is baked
//! into those colours. Each writer documents the attributes its target format preserves.

mod encode;
mod gltf;
mod obj;
mod ply;
mod stl;

use crate::MediaError;

pub use encode::{encode_glb, encode_obj};
pub use ply::encode_ply;
pub(crate) use ply::recognizes as recognizes_ply;
pub use stl::encode_stl;
pub(crate) use stl::recognizes as recognizes_stl;

/// The 3D file formats NodeCraft reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshFormat {
    /// Binary glTF 2.0.
    Glb,
    /// glTF 2.0 JSON with its buffers embedded as data URIs.
    Gltf,
    /// Wavefront OBJ (geometry only; materials are not read).
    Obj,
    /// Binary STL, with zero facet attributes and geometry only.
    Stl,
    /// NodeCraft ASCII triangle/RGBA profile; model/ply is an internal convention.
    Ply,
}

impl MeshFormat {
    /// The format's MIME type, as [`crate::sniff`] reports it.
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Glb => "model/gltf-binary",
            Self::Gltf => "model/gltf+json",
            Self::Obj => "model/obj",
            Self::Stl => "model/stl",
            Self::Ply => "model/ply",
        }
    }

    /// The format for a MIME essence, if NodeCraft reads it.
    pub fn from_mime(essence: &str) -> Option<Self> {
        [Self::Glb, Self::Gltf, Self::Obj, Self::Stl, Self::Ply].into_iter().find(|format| format.mime() == essence)
    }

    /// The usual file extension, without the dot.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Glb => "glb",
            Self::Gltf => "gltf",
            Self::Obj => "obj",
            Self::Stl => "stl",
            Self::Ply => "ply",
        }
    }
}

/// Caps on what reading one mesh may hold. The defaults admit the dense meshes 3D generators
/// return (a few hundred thousand triangles) with room to spare, and keep a decoded mesh under
/// about 300 MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshLimits {
    /// Largest encoded input, in bytes.
    pub max_input_bytes: usize,
    /// Most vertices in the decoded mesh.
    pub max_vertices: usize,
    /// Most triangles in the decoded mesh.
    pub max_triangles: usize,
    /// Most node visits while walking a glTF scene (shared instances count each time).
    pub max_node_visits: usize,
    /// Deepest glTF node hierarchy.
    pub max_depth: usize,
    /// Largest base-colour texture decoded, in pixels; a larger one is left out, not refused.
    pub max_texture_pixels: u64,
    /// Most textures decoded for one mesh; the rest are left out.
    pub max_textures: usize,
}

impl Default for MeshLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_vertices: 4_000_000,
            max_triangles: 4_000_000,
            max_node_visits: 65_536,
            max_depth: 64,
            max_texture_pixels: 4096 * 4096,
            max_textures: 16,
        }
    }
}

/// A triangulated mesh in one model space. All vectors have one entry per vertex.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    /// Unit normals. Readers compute smooth normals where a file has none.
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA colour.
    pub colors: Vec<[f32; 4]>,
    /// Three per triangle, each less than the vertex count.
    pub indices: Vec<u32>,
}

/// An axis-aligned box around a mesh.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    pub fn center(&self) -> [f32; 3] {
        [(self.min[0] + self.max[0]) * 0.5, (self.min[1] + self.max[1]) * 0.5, (self.min[2] + self.max[2]) * 0.5]
    }

    /// Half the box's diagonal: the radius of a sphere around it.
    pub fn radius(&self) -> f32 {
        let d = [self.max[0] - self.min[0], self.max[1] - self.min[1], self.max[2] - self.min[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 0.5
    }
}

/// What a mesh holds, for labels and asset metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshInfo {
    pub vertices: usize,
    pub triangles: usize,
    pub bounds: Option<Bounds>,
}

impl Mesh {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// The box around every vertex, or `None` for an empty mesh.
    pub fn bounds(&self) -> Option<Bounds> {
        let first = *self.positions.first()?;
        let mut bounds = Bounds { min: first, max: first };
        for p in &self.positions {
            for ((min, max), v) in bounds.min.iter_mut().zip(&mut bounds.max).zip(p) {
                *min = min.min(*v);
                *max = max.max(*v);
            }
        }
        Some(bounds)
    }

    pub fn info(&self) -> MeshInfo {
        MeshInfo { vertices: self.vertex_count(), triangles: self.triangle_count(), bounds: self.bounds() }
    }

    /// Checks the invariants every reader and writer relies on: matching per-vertex lengths,
    /// whole triangles, in-range indices and finite positions.
    pub fn validate(&self) -> Result<(), MediaError> {
        let n = self.positions.len();
        if self.normals.len() != n || self.colors.len() != n {
            return Err(MediaError::Decode("the mesh's vertex attributes have different lengths".into()));
        }
        if !self.indices.len().is_multiple_of(3) {
            return Err(MediaError::Decode("the mesh has a partial triangle".into()));
        }
        if self.indices.iter().any(|&i| usize::try_from(i).map_or(true, |i| i >= n)) {
            return Err(MediaError::Decode("a triangle refers to a vertex that doesn't exist".into()));
        }
        if self.positions.iter().flatten().any(|v| !v.is_finite()) {
            return Err(MediaError::Decode("a vertex position isn't a finite number".into()));
        }
        Ok(())
    }

    /// The triangles' edges, each once, as pairs of vertex indices: what a wireframe draws.
    /// `None` when there would be more than `max_edges`.
    pub fn edges(&self, max_edges: usize) -> Option<Vec<u32>> {
        let mut seen = std::collections::HashSet::new();
        let mut lines = Vec::new();
        for &[x, y, z] in self.indices.as_chunks::<3>().0 {
            for (a, b) in [(x, y), (y, z), (z, x)] {
                if seen.insert((a.min(b), a.max(b))) {
                    if seen.len() > max_edges {
                        return None;
                    }
                    lines.extend([a, b]);
                }
            }
        }
        Some(lines)
    }
}

/// Reads a mesh, recognising its format from its bytes.
pub fn decode_mesh(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    let format = crate::sniff(bytes).and_then(|s| MeshFormat::from_mime(s.mime)).ok_or(MediaError::Unrecognized)?;
    decode_mesh_as(bytes, format, limits)
}

/// Reads a mesh in a known format.
pub fn decode_mesh_as(bytes: &[u8], format: MeshFormat, limits: &MeshLimits) -> Result<Mesh, MediaError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(MediaError::TooLarge { size: bytes.len() as u64, limit: limits.max_input_bytes as u64 });
    }
    let mut mesh = match format {
        MeshFormat::Glb => gltf::decode_glb(bytes, limits)?,
        MeshFormat::Gltf => gltf::decode_gltf(bytes, limits)?,
        MeshFormat::Obj => obj::decode(bytes, limits)?,
        MeshFormat::Stl => stl::decode(bytes, limits)?,
        MeshFormat::Ply => ply::decode(bytes, limits)?,
    };
    if mesh.indices.is_empty() {
        return Err(MediaError::Decode("the file has no triangles to show".into()));
    }
    repair_normals(&mut mesh);
    mesh.validate()?;
    Ok(mesh)
}

/// The counts and bounds of a mesh, reading it in full (a mesh has no cheap header to probe).
pub fn probe_mesh(bytes: &[u8], limits: &MeshLimits) -> Result<MeshInfo, MediaError> {
    decode_mesh(bytes, limits).map(|mesh| mesh.info())
}

/// Collects decoded geometry while enforcing the vertex and triangle caps.
pub(crate) struct Builder<'a> {
    pub mesh: Mesh,
    limits: &'a MeshLimits,
}

impl<'a> Builder<'a> {
    pub fn new(limits: &'a MeshLimits) -> Self {
        Self { mesh: Mesh::default(), limits }
    }

    /// Room for `vertices` more vertices and `triangles` more triangles, or an error saying
    /// which cap they break. Nothing is reserved past the caps.
    pub fn reserve(&mut self, vertices: usize, triangles: usize) -> Result<(), MediaError> {
        let total_v = self.mesh.positions.len().checked_add(vertices).filter(|n| *n <= self.limits.max_vertices);
        let total_v = total_v.ok_or_else(|| MediaError::LimitExceeded(format!("more than {} vertices", self.limits.max_vertices)))?;
        if total_v > u32::MAX as usize {
            return Err(MediaError::LimitExceeded("too many vertices".into()));
        }
        let total_t = self.mesh.triangle_count().checked_add(triangles).filter(|n| *n <= self.limits.max_triangles);
        total_t.ok_or_else(|| MediaError::LimitExceeded(format!("more than {} triangles", self.limits.max_triangles)))?;
        self.mesh.positions.reserve(vertices);
        self.mesh.normals.reserve(vertices);
        self.mesh.colors.reserve(vertices);
        self.mesh.indices.reserve(triangles.saturating_mul(3));
        Ok(())
    }

    /// The index the next pushed vertex gets.
    pub fn base(&self) -> u32 {
        // `reserve` keeps the count within u32.
        u32::try_from(self.mesh.positions.len()).unwrap_or(u32::MAX)
    }

    pub fn vertex(&mut self, position: [f32; 3], normal: [f32; 3], color: [f32; 4]) -> Result<(), MediaError> {
        if self.mesh.positions.len() >= self.limits.max_vertices {
            return Err(MediaError::LimitExceeded(format!("more than {} vertices", self.limits.max_vertices)));
        }
        self.mesh.positions.push(position);
        self.mesh.normals.push(normal);
        self.mesh.colors.push(color);
        Ok(())
    }

    pub fn triangle(&mut self, a: u32, b: u32, c: u32) -> Result<(), MediaError> {
        if self.mesh.triangle_count() >= self.limits.max_triangles {
            return Err(MediaError::LimitExceeded(format!("more than {} triangles", self.limits.max_triangles)));
        }
        self.mesh.indices.extend([a, b, c]);
        Ok(())
    }
}

/// Unit-length normals everywhere: a missing, zero or non-finite normal is replaced by the
/// area-weighted average of its triangles' face normals (or straight up, for a lone vertex).
fn repair_normals(mesh: &mut Mesh) {
    let bad = |n: &[f32; 3]| normalize(*n).is_none();
    if !mesh.normals.iter().any(bad) {
        return;
    }
    let mut sums = vec![[0.0f32; 3]; mesh.positions.len()];
    for tri in mesh.indices.as_chunks::<3>().0 {
        let [Some(a), Some(b), Some(c)] = tri.map(|i| mesh.positions.get(i as usize)) else {
            continue;
        };
        let face = cross(sub(*b, *a), sub(*c, *a));
        for &i in tri {
            if let Some(sum) = sums.get_mut(i as usize) {
                *sum = add(*sum, face);
            }
        }
    }
    for (normal, sum) in mesh.normals.iter_mut().zip(sums) {
        if bad(normal) {
            *normal = normalize(sum).unwrap_or([0.0, 1.0, 0.0]);
        }
    }
}

pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
/// The unit vector along `v`, or `None` when it has no direction (zero, tiny or not finite).
pub(crate) fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (len.is_finite() && len > 1e-20).then(|| [v[0] / len, v[1] / len, v[2] / len])
}

#[cfg(test)]
mod stl_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod ply_tests;
