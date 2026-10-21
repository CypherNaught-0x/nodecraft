//! Writing a [`Mesh`] as binary glTF or OBJ. Both keep positions, normals and vertex colours;
//! neither writes a texture (a textured model's colours were baked into its vertices on reading).

use crate::{MediaError, mesh::Mesh};
use serde_json::json;
use std::fmt::Write as _;

/// Binary glTF 2.0: one node, one mesh, one primitive with POSITION, NORMAL, COLOR_0 (linear
/// RGBA floats) and 32-bit indices, and a plain matte material.
pub fn encode_glb(mesh: &Mesh) -> Result<Vec<u8>, MediaError> {
    mesh.validate()?;
    let bounds = mesh.bounds().ok_or(MediaError::InvalidParameter("an empty mesh can't be written"))?;
    let n = mesh.vertex_count();
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut section = |bin: &mut Vec<u8>, bytes: &mut dyn Iterator<Item = [u8; 4]>, target: u32| {
        let start = bin.len();
        for word in bytes {
            bin.extend_from_slice(&word);
        }
        views.push(json!({"buffer": 0, "byteOffset": start, "byteLength": bin.len() - start, "target": target}));
    };
    section(&mut bin, &mut mesh.positions.iter().flatten().map(|v| v.to_le_bytes()), 34962);
    section(&mut bin, &mut mesh.normals.iter().flatten().map(|v| v.to_le_bytes()), 34962);
    section(&mut bin, &mut mesh.colors.iter().flatten().map(|v| v.to_le_bytes()), 34962);
    section(&mut bin, &mut mesh.indices.iter().map(|v| v.to_le_bytes()), 34963);
    let document = json!({
        "asset": {"version": "2.0", "generator": "NodeCraft"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "COLOR_0": 2}, "indices": 3, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [1.0, 1.0, 1.0, 1.0], "metallicFactor": 0.0, "roughnessFactor": 1.0}}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": n, "type": "VEC3", "min": bounds.min, "max": bounds.max},
            {"bufferView": 1, "componentType": 5126, "count": n, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": n, "type": "VEC4"},
            {"bufferView": 3, "componentType": 5125, "count": mesh.indices.len(), "type": "SCALAR"},
        ],
        "bufferViews": views,
        "buffers": [{"byteLength": bin.len()}],
    });
    let mut json = serde_json::to_vec(&document).map_err(|e| MediaError::Decode(e.to_string()))?;
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12usize.checked_add(8 + json.len()).and_then(|n| n.checked_add(8 + bin.len()));
    let total = total.and_then(|n| u32::try_from(n).ok()).ok_or(MediaError::LimitExceeded("the mesh is too large for a GLB file".into()))?;
    let length = |bytes: &[u8]| u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes();
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&length(&json));
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&length(&bin));
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    out.extend_from_slice(&bin);
    Ok(out)
}

/// Wavefront OBJ: `v x y z r g b` (colours as displayed, sRGB), `vn` and `f v//vn` triangles.
pub fn encode_obj(mesh: &Mesh) -> Result<Vec<u8>, MediaError> {
    mesh.validate()?;
    if mesh.indices.is_empty() {
        return Err(MediaError::InvalidParameter("an empty mesh can't be written"));
    }
    let mut out = String::from("# Written by NodeCraft\n");
    for (p, c) in mesh.positions.iter().zip(&mesh.colors) {
        let _ = writeln!(out, "v {} {} {} {} {} {}", p[0], p[1], p[2], srgb(c[0]), srgb(c[1]), srgb(c[2]));
    }
    for n in &mesh.normals {
        let _ = writeln!(out, "vn {} {} {}", n[0], n[1], n[2]);
    }
    for &[a, b, c] in mesh.indices.as_chunks::<3>().0 {
        let (a, b, c) = (u64::from(a) + 1, u64::from(b) + 1, u64::from(c) + 1);
        let _ = writeln!(out, "f {a}//{a} {b}//{b} {c}//{c}");
    }
    Ok(out.into_bytes())
}

/// A linear colour channel as a displayed (sRGB) value, rounded to three decimals.
fn srgb(v: f32) -> f32 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let encoded = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (encoded * 1000.0).round() / 1000.0
}
