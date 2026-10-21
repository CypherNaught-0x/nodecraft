//! Wavefront OBJ geometry: `v` (with the common trailing `r g b` vertex colour), `vn`, `vt` and
//! `f` with any of the `v`, `v/vt`, `v//vn` and `v/vt/vn` corner forms, negative (relative)
//! indices included. Faces with more than three corners are fanned into triangles. Materials
//! (`mtllib`, `usemtl`) name other files and are not read; groups, smoothing, lines and points are
//! skipped.

use super::{Builder, MeshLimits};
use crate::{MediaError, mesh::Mesh};
use std::collections::HashMap;

/// The colour of a vertex that doesn't give one (linear light grey).
const DEFAULT_COLOR: [f32; 4] = [0.6, 0.6, 0.6, 1.0];

pub(super) fn decode(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    let mut positions: Vec<([f32; 3], [f32; 4])> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut builder = Builder::new(limits);
    // One output vertex per distinct (position, normal) pair a face corner names.
    let mut corners: HashMap<(usize, Option<usize>), u32> = HashMap::new();
    let mut face: Vec<u32> = Vec::new();
    for (number, raw) in bytes.split(|&b| b == b'\n').enumerate() {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw).trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let line = std::str::from_utf8(line).map_err(|_| error(number, "isn't text"))?;
        let mut words = line.split_ascii_whitespace();
        let Some(keyword) = words.next() else { continue };
        match keyword {
            "v" => {
                if positions.len() >= limits.max_vertices {
                    return Err(MediaError::LimitExceeded(format!("more than {} vertices", limits.max_vertices)));
                }
                let values = numbers(words, number)?;
                let (position, color) = match values.as_slice() {
                    [x, y, z] | [x, y, z, _] => ([*x, *y, *z], DEFAULT_COLOR),
                    [x, y, z, r, g, b] | [x, y, z, r, g, b, _] => ([*x, *y, *z], [srgb(*r), srgb(*g), srgb(*b), 1.0]),
                    _ => return Err(error(number, "has a vertex without three coordinates")),
                };
                if position.iter().any(|v| !v.is_finite()) {
                    return Err(error(number, "has a vertex that isn't a finite number"));
                }
                positions.push((position, color));
            }
            "vn" => {
                if normals.len() >= limits.max_vertices {
                    return Err(MediaError::LimitExceeded(format!("more than {} normals", limits.max_vertices)));
                }
                match numbers(words, number)?.as_slice() {
                    [x, y, z] => normals.push(super::normalize([*x, *y, *z]).unwrap_or([0.0; 3])),
                    _ => return Err(error(number, "has a normal without three coordinates")),
                }
            }
            "f" => {
                face.clear();
                for corner in words {
                    let (position, normal) = parse_corner(corner, positions.len(), normals.len())
                        .ok_or_else(|| error(number, "has a face corner that doesn't refer to a vertex"))?;
                    let index = match corners.get(&(position, normal)) {
                        Some(index) => *index,
                        None => {
                            let index = builder.base();
                            let (point, color) = positions.get(position).copied().ok_or_else(|| error(number, "refers to a missing vertex"))?;
                            let n = normal.and_then(|i| normals.get(i).copied()).unwrap_or([0.0; 3]);
                            builder.vertex(point, n, color)?;
                            corners.insert((position, normal), index);
                            index
                        }
                    };
                    face.push(index);
                }
                let Some((&first, rest)) = face.split_first() else {
                    return Err(error(number, "has an empty face"));
                };
                if rest.len() < 2 {
                    return Err(error(number, "has a face with fewer than three corners"));
                }
                for pair in rest.windows(2) {
                    if let [b, c] = pair {
                        builder.triangle(first, *b, *c)?;
                    }
                }
            }
            // Texture coordinates only matter with a texture, which would be another file; groups,
            // smoothing, materials, lines, points and free-form statements are skipped too.
            _ => {}
        }
    }
    Ok(builder.mesh)
}

fn numbers<'a>(words: impl Iterator<Item = &'a str>, number: usize) -> Result<Vec<f32>, MediaError> {
    let mut values = Vec::new();
    for word in words {
        if values.len() >= 8 {
            return Err(error(number, "has too many numbers"));
        }
        values.push(word.parse::<f32>().map_err(|_| error(number, "has something that isn't a number"))?);
    }
    Ok(values)
}

/// Zero-based (position, normal) indices for one `f` corner. OBJ indices start at 1; negative
/// ones count back from the latest element read so far.
fn parse_corner(corner: &str, positions: usize, normals: usize) -> Option<(usize, Option<usize>)> {
    let mut parts = corner.split('/');
    let position = resolve(parts.next()?, positions)?;
    let _texture = parts.next();
    let normal = match parts.next() {
        None | Some("") => None,
        Some(text) => Some(resolve(text, normals)?),
    };
    if parts.next().is_some() {
        return None;
    }
    Some((position, normal))
}

fn resolve(text: &str, count: usize) -> Option<usize> {
    let value: i64 = text.parse().ok()?;
    let index = if value > 0 {
        usize::try_from(value - 1).ok()?
    } else if value < 0 {
        count.checked_sub(usize::try_from(value.checked_neg()?).ok()?)?
    } else {
        return None;
    };
    (index < count).then_some(index)
}

/// OBJ vertex colours are written as displayed (sRGB) values.
fn srgb(v: f32) -> f32 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn error(line: usize, what: &str) -> MediaError {
    MediaError::Decode(format!("line {} {what}", line.saturating_add(1)))
}
