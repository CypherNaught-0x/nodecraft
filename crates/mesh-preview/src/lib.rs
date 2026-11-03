//! Bounded CPU preparation for trusted 3D preview workers (#1013).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use nodecraft_media::{Bounds, MeshLimits, decode_mesh};
use std::collections::HashSet;

pub const VERTEX_BYTES: usize = 40;
pub const MAX_INPUT: usize = 16 * 1024 * 1024;
pub const ADOPT_BYTES: usize = 256 * 1024;
pub const PROTOCOL: u32 = 1;
pub const BUILD: &str = env!("NODECRAFT_MESH_BUILD");
pub const LOADER: &str = "nodecraft-mesh-worker_loader.js";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Coordinates,
    TooLarge,
    Capacity,
    Unavailable,
    Timeout,
    Cancelled,
    Protocol,
}
impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid => "Can't read this 3D model",
            Self::Coordinates => "3D preview unavailable at this coordinate range",
            Self::TooLarge => "This 3D model is too large to preview",
            Self::Capacity => "Not enough memory for this 3D preview",
            Self::Timeout => "3D preview timed out; reload to retry",
            Self::Unavailable => "3D preview unavailable; reload to retry",
            Self::Cancelled => "3D preview cancelled",
            Self::Protocol => "3D preview version mismatch; reload to retry",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Native,
    Browser,
}
impl Profile {
    pub fn limits(self) -> MeshLimits {
        match self {
            Self::Native => MeshLimits::default(),
            Self::Browser => MeshLimits {
                max_input_bytes: MAX_INPUT,
                max_vertices: 600_000,
                max_triangles: 300_000,
                max_node_visits: 8192,
                max_depth: 32,
                max_texture_pixels: 1024 * 1024,
                max_textures: 4,
                max_texture_alloc: 32 * 1024 * 1024,
                max_json_values: 250_000,
                max_json_objects: 16_384,
            },
        }
    }
    pub fn edges(self) -> usize {
        match self {
            Self::Native => 6_000_000,
            Self::Browser => 900_000,
        }
    }
}

/// Sizes admitted before any geometry or transfer allocation. Counts are never obtained by
/// truncating JavaScript numbers; the wire adapter admits finite integral u32 values first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub vertices: u32,
    pub triangles: u32,
    pub edge_words: u32,
}
impl Layout {
    pub fn sizes(self, profile: Profile) -> Result<[usize; 3], Error> {
        let limits = profile.limits();
        let v = usize::try_from(self.vertices).map_err(|_| Error::TooLarge)?;
        let t = usize::try_from(self.triangles).map_err(|_| Error::TooLarge)?;
        let e = usize::try_from(self.edge_words).map_err(|_| Error::TooLarge)?;
        if v == 0 || t == 0 || v > limits.max_vertices || t > limits.max_triangles || !e.is_multiple_of(2) || e / 2 > profile.edges() {
            return Err(Error::TooLarge);
        }
        Ok([v.checked_mul(VERTEX_BYTES).ok_or(Error::TooLarge)?, t.checked_mul(12).ok_or(Error::TooLarge)?, e.checked_mul(4).ok_or(Error::TooLarge)?])
    }
}

/// Validated, little-endian GPU bytes. No vertex/index packing is needed during paint.
#[derive(Debug)]
pub struct Prepared {
    layout: Layout,
    buffers: [Vec<u8>; 3],
    bounds: Bounds,
    center: [f32; 3],
    radius: f32,
    fingerprint: [u8; 32],
}
impl Prepared {
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    pub fn buffers(&self) -> [&[u8]; 3] {
        self.buffers.each_ref().map(Vec::as_slice)
    }
    pub fn bytes(&self) -> usize {
        self.buffers.iter().map(Vec::len).sum()
    }
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }
    pub fn center(&self) -> [f32; 3] {
        self.center
    }
    pub fn radius(&self) -> f32 {
        self.radius
    }
    pub fn into_buffers(self) -> [Vec<u8>; 3] {
        self.buffers
    }
}

fn view(bounds: Bounds) -> Result<([f32; 3], f32), Error> {
    let mut center = [0.0; 3];
    let mut square = 0.0f64;
    for ((out, min), max) in center.iter_mut().zip(bounds.min).zip(bounds.max) {
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(Error::Invalid);
        }
        *out = ((f64::from(min) + f64::from(max)) * 0.5) as f32;
        square += (f64::from(max) - f64::from(min)).powi(2);
    }
    let radius = (square.sqrt() * 0.5) as f32;
    if center.iter().any(|v| !v.is_finite()) || !radius.is_finite() || radius <= 0.0 {
        return Err(Error::Invalid);
    }
    Ok((center, radius))
}
#[cfg(feature = "conformance")]
thread_local! {static RESERVES:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
#[cfg(feature = "conformance")]
pub fn reserved_for_conformance() -> usize {
    RESERVES.with(std::cell::Cell::get)
}
fn reserve(n: usize) -> Result<Vec<u8>, Error> {
    #[cfg(feature = "conformance")]
    RESERVES.with(|value| value.set(value.get().saturating_add(n)));

    let mut out = Vec::new();
    out.try_reserve_exact(n).map_err(|_| Error::Capacity)?;
    Ok(out)
}

pub fn prepare(bytes: &[u8], profile: Profile) -> Result<Prepared, Error> {
    prepare_checked(bytes, profile, |_, _| true)
}

/// Checks the caller's renderer capability before allocating edges or packed buffers.
/// Geometry interchange remains valid even when a preview camera cannot represent it.
pub fn prepare_checked(bytes: &[u8], profile: Profile, admit: impl FnOnce([f32; 3], f32) -> bool) -> Result<Prepared, Error> {
    if bytes.is_empty() || bytes.len() > profile.limits().max_input_bytes {
        return Err(Error::TooLarge);
    }
    let mesh = decode_mesh(bytes, &profile.limits()).map_err(|e| match e {
        nodecraft_media::MediaError::TooLarge { .. } | nodecraft_media::MediaError::LimitExceeded(_) => Error::TooLarge,
        _ => Error::Invalid,
    })?;
    let bounds = mesh.bounds().ok_or(Error::Invalid)?;
    let (center, radius) = view(bounds)?;
    if !admit(center, radius) {
        return Err(Error::Coordinates);
    }
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    let mut overflow = false;
    'triangles: for &[x, y, z] in mesh.indices.as_chunks::<3>().0 {
        for (a, b) in [(x, y), (y, z), (z, x)] {
            let pair = (a.min(b), a.max(b));
            if !seen.contains(&pair) {
                if seen.len() >= profile.edges() {
                    overflow = true;
                    break 'triangles;
                }
                seen.try_reserve(1).map_err(|_| Error::Capacity)?;
                lines.try_reserve(2).map_err(|_| Error::Capacity)?;
                seen.insert(pair);
                lines.extend([a, b]);
            }
        }
    }
    drop(seen);
    if overflow {
        lines.clear();
    }
    let layout = Layout {
        vertices: u32::try_from(mesh.vertex_count()).map_err(|_| Error::TooLarge)?,
        triangles: u32::try_from(mesh.triangle_count()).map_err(|_| Error::TooLarge)?,
        edge_words: u32::try_from(lines.len()).map_err(|_| Error::TooLarge)?,
    };
    let sizes = layout.sizes(profile)?;
    let mut buffers = [reserve(sizes[0])?, reserve(sizes[1])?, reserve(sizes[2])?];
    for ((position, normal), color) in mesh.positions.iter().zip(&mesh.normals).zip(&mesh.colors) {
        for &v in position.iter().chain(normal).chain(color) {
            if !v.is_finite() {
                return Err(Error::Invalid);
            }
            buffers[0].extend_from_slice(&v.to_le_bytes());
        }
    }
    for &i in &mesh.indices {
        buffers[1].extend_from_slice(&i.to_le_bytes());
    }
    for &i in &lines {
        buffers[2].extend_from_slice(&i.to_le_bytes());
    }
    let mut digest = blake3::Hasher::new();
    for bytes in &buffers {
        digest.update(bytes);
    }
    Ok(Prepared { layout, buffers, bounds, center, radius, fingerprint: *digest.finalize().as_bytes() })
}

/// A reply is validated incrementally before it becomes a renderable Prepared. Every push is
/// at most ADOPT_BYTES and aligned to whole records. Untrusted messages cannot demand an
/// unbounded typed-array-to-Vec copy or one full-model validation on a frame.
pub struct Adoption {
    layout: Layout,
    sizes: [usize; 3],
    buffers: [Vec<u8>; 3],
    bounds: Bounds,
    measured: Option<Bounds>,
    center: [f32; 3],
    radius: f32,
    digest: blake3::Hasher,
}
impl Adoption {
    pub fn new(layout: Layout, bounds: Bounds, extents: [usize; 3]) -> Result<Self, Error> {
        let sizes = layout.sizes(Profile::Browser)?;
        if sizes != extents {
            return Err(Error::Protocol);
        }
        let (center, radius) = view(bounds)?;
        Ok(Self {
            layout,
            sizes,
            buffers: [reserve(sizes[0])?, reserve(sizes[1])?, reserve(sizes[2])?],
            bounds,
            measured: None,
            center,
            radius,
            digest: blake3::Hasher::new(),
        })
    }
    pub fn progress(&self) -> Option<(usize, usize, usize)> {
        self.buffers.iter().zip(self.sizes).enumerate().find_map(|(i, (out, n))| (out.len() < n).then_some((i, out.len(), n)))
    }
    pub fn push(&mut self, buffer: usize, bytes: &[u8]) -> Result<(), Error> {
        let Some((next, at, total)) = self.progress() else {
            return Err(Error::Protocol);
        };
        let record = if buffer == 0 { VERTEX_BYTES } else { 4 };
        if buffer != next
            || bytes.is_empty()
            || bytes.len() > ADOPT_BYTES
            || !bytes.len().is_multiple_of(record)
            || at.checked_add(bytes.len()).is_none_or(|n| n > total)
        {
            return Err(Error::Protocol);
        }
        if buffer == 0 {
            for vertex in bytes.as_chunks::<VERTEX_BYTES>().0 {
                let mut position = [0.0; 3];
                for (i, word) in vertex.as_chunks::<4>().0.iter().enumerate() {
                    let value = f32::from_le_bytes(*word);
                    if !value.is_finite() {
                        return Err(Error::Invalid);
                    }
                    if let Some(out) = position.get_mut(i) {
                        *out = value;
                    }
                }
                match &mut self.measured {
                    None => self.measured = Some(Bounds { min: position, max: position }),
                    Some(bounds) => {
                        for ((min, max), value) in bounds.min.iter_mut().zip(&mut bounds.max).zip(position) {
                            *min = min.min(value);
                            *max = max.max(value);
                        }
                    }
                }
            }
        } else if bytes.as_chunks::<4>().0.iter().any(|word| u32::from_le_bytes(*word) >= self.layout.vertices) {
            return Err(Error::Invalid);
        }
        self.digest.update(bytes);
        self.buffers.get_mut(buffer).ok_or(Error::Protocol)?.extend_from_slice(bytes);
        Ok(())
    }
    pub fn finish(self) -> Result<Prepared, Error> {
        if self.progress().is_some() || self.measured != Some(self.bounds) {
            return Err(Error::Protocol);
        }
        Ok(Prepared {
            layout: self.layout,
            buffers: self.buffers,
            bounds: self.bounds,
            center: self.center,
            radius: self.radius,
            fingerprint: *self.digest.finalize().as_bytes(),
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub mod browser;
#[cfg(test)]
mod tests;
#[cfg(target_arch = "wasm32")]
mod wire;
#[cfg(target_arch = "wasm32")]
pub mod worker;

#[cfg(any(test, feature = "conformance"))]
pub mod fixture;
