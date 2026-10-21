//! glTF 2.0: binary (`.glb`) or JSON (`.gltf`) with embedded (data URI) buffers and images.
//!
//! The default scene is walked from its root nodes, each mesh primitive's triangles are moved into
//! model space by its node's world transform, and the material's base colour (factor, vertex
//! colour and texture) becomes a vertex colour. Sparse accessors, compressed geometry (Draco,
//! meshopt) and morph targets are not read; a file that requires an extension NodeCraft doesn't
//! know says which one.

use super::{Builder, MeshLimits};
use crate::{Limits, MediaError, mesh::Mesh};
use serde::Deserialize;
use std::{borrow::Cow, cell::Cell, collections::HashMap};

const JSON_CHUNK: u32 = 0x4E4F_534A;
const BIN_CHUNK: u32 = 0x004E_4942;
/// Extensions a file may require that change nothing NodeCraft draws, or that it reads anyway.
const KNOWN_REQUIRED: &[&str] = &["KHR_mesh_quantization", "KHR_materials_unlit", "KHR_materials_emissive_strength"];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Most JSON values (counted by their separators) a file may hold. Typed parsing can grow a
/// number into eight bytes, so this, not the input size, bounds what the JSON tree costs.
const MAX_JSON_VALUES: usize = 4_000_000;
/// Most JSON objects a file may hold: each becomes a typed struct of up to a few hundred bytes,
/// so a few megabytes of `{},` can't grow into hundreds of megabytes.
const MAX_JSON_OBJECTS: usize = 262_144;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Root {
    asset: Asset,
    scene: Option<usize>,
    #[serde(default)]
    scenes: Vec<Scene>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    meshes: Vec<MeshJson>,
    #[serde(default)]
    accessors: Vec<Accessor>,
    #[serde(default)]
    buffer_views: Vec<BufferView>,
    #[serde(default)]
    buffers: Vec<Buffer>,
    #[serde(default)]
    materials: Vec<Material>,
    #[serde(default)]
    textures: Vec<Texture>,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    extensions_required: Vec<String>,
}

#[derive(Deserialize)]
struct Asset {
    version: String,
}

#[derive(Deserialize)]
struct Scene {
    #[serde(default)]
    nodes: Vec<usize>,
}

#[derive(Deserialize)]
struct Node {
    #[serde(default)]
    children: Vec<usize>,
    mesh: Option<usize>,
    matrix: Option<[f32; 16]>,
    translation: Option<[f32; 3]>,
    rotation: Option<[f32; 4]>,
    scale: Option<[f32; 3]>,
}

#[derive(Deserialize)]
struct MeshJson {
    #[serde(default)]
    primitives: Vec<Primitive>,
}

#[derive(Deserialize)]
struct Primitive {
    #[serde(default)]
    attributes: HashMap<String, usize>,
    indices: Option<usize>,
    material: Option<usize>,
    mode: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Accessor {
    buffer_view: Option<usize>,
    #[serde(default)]
    byte_offset: u64,
    component_type: u32,
    #[serde(default)]
    normalized: bool,
    count: u64,
    #[serde(rename = "type")]
    kind: String,
    sparse: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BufferView {
    buffer: usize,
    #[serde(default)]
    byte_offset: u64,
    byte_length: u64,
    byte_stride: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Buffer {
    byte_length: u64,
    uri: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Material {
    pbr_metallic_roughness: Option<Pbr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pbr {
    base_color_factor: Option<[f32; 4]>,
    base_color_texture: Option<TextureInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextureInfo {
    index: usize,
    #[serde(default)]
    tex_coord: u32,
}

#[derive(Deserialize)]
struct Texture {
    source: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Image {
    uri: Option<String>,
    buffer_view: Option<usize>,
}

/// A buffer's bytes, or why there are none.
enum Data<'a> {
    Bytes(Cow<'a, [u8]>),
    External,
    Missing,
}

/// A decoded base-colour texture.
struct Rgba {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct Reader<'a> {
    root: Root,
    buffers: Vec<Data<'a>>,
    limits: &'a MeshLimits,
    /// Decoded textures by image index; `None` when one couldn't be (it is left out).
    textures: HashMap<usize, Option<Rgba>>,
    /// Primitives drawn so far, counted at every instance; shares the node-visit cap.
    primitives: usize,
    /// Accessor values decoded so far. Shared instances decode their accessors again, and an
    /// unused attribute is decoded before it is dropped, so this, not the output, bounds the work.
    decoded: Cell<usize>,
}

/// A GLB chunk: its type, its payload and where the next one starts.
type Chunk<'a> = (u32, &'a [u8], usize);

pub(super) fn decode_glb(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    let word = |at: usize| bytes.get(at..at.checked_add(4)?).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    if bytes.get(..4) != Some(b"glTF") {
        return Err(MediaError::Unrecognized);
    }
    if word(4) != Some(2) {
        return Err(MediaError::UnsupportedFeature("only glTF 2.0 files can be read".into()));
    }
    let declared = word(8).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| corrupt("its header is cut short"))?;
    let body = bytes.get(..declared).filter(|_| declared >= 12).ok_or_else(|| corrupt("it is shorter than its header says"))?;
    let chunk = |at: usize| -> Result<Option<Chunk<'_>>, MediaError> {
        if at >= body.len() {
            return Ok(None);
        }
        let length = body.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).ok_or_else(|| corrupt("a chunk header is cut short"))?;
        let kind = body.get(at + 4..at + 8).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).ok_or_else(|| corrupt("a chunk header is cut short"))?;
        let start = at + 8;
        let end = usize::try_from(length).ok().and_then(|n| start.checked_add(n)).ok_or_else(|| corrupt("a chunk is too long"))?;
        let data = body.get(start..end).ok_or_else(|| corrupt("a chunk runs past the end of the file"))?;
        // Chunks are padded to four bytes.
        Ok(Some((kind, data, end.checked_add(3).map_or(end, |n| n & !3))))
    };
    let (kind, json, next) = chunk(12)?.ok_or_else(|| corrupt("it has no JSON chunk"))?;
    if kind != JSON_CHUNK {
        return Err(corrupt("its first chunk isn't JSON"));
    }
    let bin = match chunk(next)? {
        Some((BIN_CHUNK, data, _)) => Some(data),
        _ => None,
    };
    Reader::new(json, bin, limits)?.read()
}

pub(super) fn decode_gltf(bytes: &[u8], limits: &MeshLimits) -> Result<Mesh, MediaError> {
    Reader::new(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes), None, limits)?.read()
}

impl<'a> Reader<'a> {
    fn new(json: &[u8], bin: Option<&'a [u8]>, limits: &'a MeshLimits) -> Result<Self, MediaError> {
        let (values, objects) = json_values(json);
        if values > MAX_JSON_VALUES || objects > MAX_JSON_OBJECTS {
            return Err(MediaError::LimitExceeded("the glTF JSON holds too many values".into()));
        }
        let root: Root = serde_json::from_slice(json).map_err(|e| corrupt(&format!("its JSON is not valid glTF ({e})")))?;
        if !root.asset.version.starts_with("2.") {
            return Err(MediaError::UnsupportedFeature("only glTF 2.0 files can be read".into()));
        }
        if let Some(name) = root.extensions_required.iter().find(|name| !KNOWN_REQUIRED.contains(&name.as_str())) {
            let name: String = name.chars().take(64).collect();
            return Err(MediaError::UnsupportedFeature(format!("this glTF needs the {name} extension, which NodeCraft can't read yet")));
        }
        let mut buffers = Vec::with_capacity(root.buffers.len().min(1024));
        for (index, buffer) in root.buffers.iter().enumerate() {
            let data = match &buffer.uri {
                None if index == 0 => bin.map_or(Data::Missing, |b| Data::Bytes(Cow::Borrowed(b))),
                None => Data::Missing,
                Some(uri) => match data_uri(uri, limits.max_input_bytes)? {
                    Some(bytes) => Data::Bytes(Cow::Owned(bytes)),
                    None => Data::External,
                },
            };
            if let Data::Bytes(bytes) = &data
                && (bytes.len() as u64) < buffer.byte_length
            {
                return Err(corrupt("a buffer is shorter than it says"));
            }
            buffers.push(data);
        }
        Ok(Self { root, buffers, limits, textures: HashMap::new(), primitives: 0, decoded: Cell::new(0) })
    }

    fn read(mut self) -> Result<Mesh, MediaError> {
        let mut builder = Builder::new(self.limits);
        let mut stack: Vec<(usize, [f32; 16], usize)> = self.roots().into_iter().rev().map(|node| (node, IDENTITY, 0)).collect();
        let mut visits = 0usize;
        if self.root.nodes.is_empty() {
            // Meshes with no scene graph at all are drawn where they are.
            for mesh in 0..self.root.meshes.len() {
                self.mesh(mesh, &IDENTITY, &mut builder)?;
            }
        }
        while let Some((index, parent, depth)) = stack.pop() {
            visits += 1;
            if visits > self.limits.max_node_visits {
                return Err(MediaError::LimitExceeded(format!("more than {} scene nodes", self.limits.max_node_visits)));
            }
            if depth > self.limits.max_depth {
                return Err(MediaError::LimitExceeded("the scene's nodes nest too deeply (or loop)".into()));
            }
            let node = self.root.nodes.get(index).ok_or_else(|| corrupt("a node refers to a node that doesn't exist"))?;
            let world = mul(&parent, &local(node));
            let (mesh, children) = (node.mesh, node.children.clone());
            if let Some(mesh) = mesh {
                self.mesh(mesh, &world, &mut builder)?;
            }
            for child in children.into_iter().rev() {
                if stack.len() >= self.limits.max_node_visits {
                    return Err(MediaError::LimitExceeded(format!("more than {} scene nodes", self.limits.max_node_visits)));
                }
                stack.push((child, world, depth + 1));
            }
        }
        Ok(builder.mesh)
    }

    /// The nodes the default scene starts from; without scenes, every node no other node lists
    /// as a child.
    fn roots(&self) -> Vec<usize> {
        let scene = self.root.scene.unwrap_or(0);
        if let Some(scene) = self.root.scenes.get(scene) {
            return scene.nodes.clone();
        }
        let mut child = vec![false; self.root.nodes.len()];
        for node in &self.root.nodes {
            for &c in &node.children {
                if let Some(flag) = child.get_mut(c) {
                    *flag = true;
                }
            }
        }
        (0..self.root.nodes.len()).filter(|i| !child.get(*i).copied().unwrap_or(false)).collect()
    }

    fn mesh(&mut self, index: usize, world: &[f32; 16], builder: &mut Builder<'_>) -> Result<(), MediaError> {
        let count = self.root.meshes.get(index).ok_or_else(|| corrupt("a node refers to a mesh that doesn't exist"))?.primitives.len();
        for primitive in 0..count {
            self.primitive(index, primitive, world, builder)?;
        }
        Ok(())
    }

    fn primitive(&mut self, mesh: usize, index: usize, world: &[f32; 16], builder: &mut Builder<'_>) -> Result<(), MediaError> {
        self.primitives += 1;
        if self.primitives > self.limits.max_node_visits {
            return Err(MediaError::LimitExceeded(format!("more than {} mesh primitives", self.limits.max_node_visits)));
        }
        let Some(primitive) = self.root.meshes.get(mesh).and_then(|m| m.primitives.get(index)) else {
            return Ok(());
        };
        let mode = primitive.mode.unwrap_or(4);
        if !matches!(mode, 4..=6) {
            // Points and lines have no surface to show.
            return Ok(());
        }
        let Some(&position) = primitive.attributes.get("POSITION") else {
            return Ok(());
        };
        let (normal, color, indices, material) =
            (primitive.attributes.get("NORMAL").copied(), primitive.attributes.get("COLOR_0").copied(), primitive.indices, primitive.material);
        let positions = self.floats(position, &[3])?;
        let n = positions.len() / 3;
        let normals = match normal {
            Some(a) => Some(self.floats(a, &[3])?).filter(|v| v.len() == n * 3),
            None => None,
        };
        let colors = match color {
            Some(a) => self.colors(a, n)?,
            None => None,
        };
        let (factor, texture) = self.material(material);
        let texcoords = match texture {
            Some((_, set)) => match self.root.meshes.get(mesh).and_then(|m| m.primitives.get(index)).and_then(|p| p.attributes.get(&format!("TEXCOORD_{set}")))
            {
                Some(&a) => Some(self.floats(a, &[2])?).filter(|v| v.len() == n * 2),
                None => None,
            },
            None => None,
        };
        let image = texture.map(|(image, _)| image);
        if let Some(image) = image {
            self.texture(image);
        }
        let order: Vec<u32> = match indices {
            Some(a) => self.indices(a)?,
            None => (0..u32::try_from(n).map_err(|_| MediaError::LimitExceeded("too many vertices".into()))?).collect(),
        };
        let triangles = triangulate(&order, mode);
        builder.reserve(n, triangles.len() / 3)?;
        let base = builder.base();
        let normal_matrix = normal_matrix(world);
        let flip = det3(world) < 0.0;
        let tex = image.and_then(|i| self.textures.get(&i)).and_then(Option::as_ref);
        for v in 0..n {
            let p = triple(&positions, v);
            let normal = normals.as_ref().map_or([0.0; 3], |all| triple(all, v));
            let mut rgba = factor;
            if let Some(colors) = &colors {
                rgba = modulate(rgba, colors.get(v).copied().unwrap_or(WHITE));
            }
            if let (Some(tex), Some(uv)) = (tex, &texcoords) {
                rgba = modulate(rgba, sample(tex, uv.get(v * 2).copied().unwrap_or(0.0), uv.get(v * 2 + 1).copied().unwrap_or(0.0)));
            }
            let world_p = point(world, p);
            if world_p.iter().any(|c| !c.is_finite()) {
                return Err(corrupt("a vertex position isn't a finite number"));
            }
            // The cofactor matrix is det·M⁻ᵀ, so a mirroring transform turns normals inside out.
            let normal = apply3(&normal_matrix, normal).map(|c| if flip { -c } else { c });
            builder.vertex(world_p, super::normalize(normal).unwrap_or([0.0; 3]), rgba)?;
        }
        for &[a, b, c] in triangles.as_chunks::<3>().0 {
            if [a, b, c].iter().any(|&i| i as usize >= n) {
                return Err(corrupt("a triangle refers to a vertex that doesn't exist"));
            }
            let (b, c) = if flip { (c, b) } else { (b, c) };
            builder.triangle(base + a, base + b, base + c)?;
        }
        Ok(())
    }

    /// The material's base colour factor, and its base-colour texture's image and UV set.
    fn material(&self, material: Option<usize>) -> ([f32; 4], Option<(usize, u32)>) {
        let Some(pbr) = material.and_then(|m| self.root.materials.get(m)).and_then(|m| m.pbr_metallic_roughness.as_ref()) else {
            return (WHITE, None);
        };
        let factor = pbr.base_color_factor.filter(|f| f.iter().all(|v| v.is_finite())).unwrap_or(WHITE);
        let texture = pbr.base_color_texture.as_ref().and_then(|info| Some((self.root.textures.get(info.index)?.source?, info.tex_coord)));
        (factor, texture)
    }

    /// Decodes a texture once; one that can't be decoded, or is past the limits, is left out.
    fn texture(&mut self, image: usize) {
        if self.textures.contains_key(&image) {
            return;
        }
        let decoded = if self.textures.len() >= self.limits.max_textures { None } else { self.decode_texture(image) };
        self.textures.insert(image, decoded);
    }

    fn decode_texture(&self, image: usize) -> Option<Rgba> {
        let entry = self.root.images.get(image)?;
        let bytes: Cow<'_, [u8]> = match (&entry.uri, entry.buffer_view) {
            (_, Some(view)) => Cow::Borrowed(self.view(view).ok()?),
            (Some(uri), None) => Cow::Owned(data_uri(uri, self.limits.max_input_bytes).ok()??),
            (None, None) => return None,
        };
        let side = u32::try_from(self.limits.max_texture_pixels.isqrt()).unwrap_or(u32::MAX).max(1);
        let limits = Limits {
            max_input_bytes: self.limits.max_input_bytes as u64,
            max_width: side.saturating_mul(4),
            max_height: side.saturating_mul(4),
            max_alloc: 512 * 1024 * 1024,
        };
        let meta = crate::probe_image(&bytes, &limits).ok()?;
        if u64::from(meta.width).saturating_mul(u64::from(meta.height)) > self.limits.max_texture_pixels {
            return None;
        }
        let (_, decoded) = crate::decode_image(&bytes, &limits).ok()?;
        let rgba = decoded.to_rgba8();
        Some(Rgba { width: rgba.width(), height: rgba.height(), pixels: rgba.into_raw() })
    }

    /// COLOR_0 as linear RGBA, when it has one entry per vertex.
    fn colors(&self, accessor: usize, n: usize) -> Result<Option<Vec<[f32; 4]>>, MediaError> {
        let components = self.components(accessor)?;
        let values = self.floats(accessor, &[3, 4])?;
        if values.len() != n * components {
            return Ok(None);
        }
        Ok(Some(
            values
                .chunks_exact(components)
                .map(|c| {
                    let channel = |i: usize| c.get(i).copied().filter(|v| v.is_finite()).map_or(1.0, |v| v.clamp(0.0, 1.0));
                    [channel(0), channel(1), channel(2), channel(3)]
                })
                .collect(),
        ))
    }

    fn accessor(&self, index: usize) -> Result<&Accessor, MediaError> {
        self.root.accessors.get(index).ok_or_else(|| corrupt("a mesh refers to an accessor that doesn't exist"))
    }

    fn components(&self, index: usize) -> Result<usize, MediaError> {
        match self.accessor(index)?.kind.as_str() {
            "SCALAR" => Ok(1),
            "VEC2" => Ok(2),
            "VEC3" => Ok(3),
            "VEC4" => Ok(4),
            _ => Err(MediaError::UnsupportedFeature("matrix vertex attributes can't be read".into())),
        }
    }

    /// The bytes of a buffer view, checked against its buffer.
    fn view(&self, index: usize) -> Result<&[u8], MediaError> {
        let view = self.root.buffer_views.get(index).ok_or_else(|| corrupt("an accessor refers to a buffer view that doesn't exist"))?;
        let buffer = match self.buffers.get(view.buffer) {
            Some(Data::Bytes(bytes)) => bytes.as_ref(),
            Some(Data::External) => {
                return Err(MediaError::UnsupportedFeature(
                    "this glTF keeps its data in a separate file; NodeCraft only reads glTF with embedded data (export it as .glb)".into(),
                ));
            }
            Some(Data::Missing) => return Err(corrupt("a buffer has no data")),
            None => return Err(corrupt("a buffer view refers to a buffer that doesn't exist")),
        };
        let start = usize::try_from(view.byte_offset).map_err(|_| corrupt("a buffer view starts past its buffer"))?;
        let end = usize::try_from(view.byte_length).ok().and_then(|n| start.checked_add(n)).ok_or_else(|| corrupt("a buffer view runs past its buffer"))?;
        buffer.get(start..end).ok_or_else(|| corrupt("a buffer view runs past its buffer"))
    }

    /// An accessor's elements and stride: (bytes, stride, element size, component size).
    fn elements(&self, index: usize, components: usize) -> Result<(&[u8], usize, usize, usize, &Accessor), MediaError> {
        let accessor = self.accessor(index)?;
        if accessor.sparse.is_some() {
            return Err(MediaError::UnsupportedFeature("sparse glTF accessors can't be read yet".into()));
        }
        let size = component_size(accessor.component_type).ok_or_else(|| corrupt("an accessor has an unknown component type"))?;
        let element = size * components;
        let count = usize::try_from(accessor.count).map_err(|_| too_many())?;
        let cap = self.limits.max_vertices.max(self.limits.max_triangles.saturating_mul(3));
        if count > cap {
            return Err(too_many());
        }
        let Some(view_index) = accessor.buffer_view else {
            return Err(MediaError::UnsupportedFeature("accessors without data can't be read".into()));
        };
        let stride = match self.root.buffer_views.get(view_index).and_then(|v| v.byte_stride) {
            Some(stride) => {
                usize::try_from(stride).ok().filter(|s| *s >= element && *s <= 252).ok_or_else(|| corrupt("a buffer view has an invalid stride"))?
            }
            None => element,
        };
        let view = self.view(view_index)?;
        let offset = usize::try_from(accessor.byte_offset).map_err(|_| corrupt("an accessor starts past its buffer view"))?;
        if count > 0 {
            let last = (count - 1).checked_mul(stride).and_then(|n| n.checked_add(offset)).and_then(|n| n.checked_add(element));
            if last.is_none_or(|end| end > view.len()) {
                return Err(corrupt("an accessor runs past its buffer view"));
            }
        }
        let bytes = view.get(offset..).unwrap_or_default();
        Ok((bytes, stride, element, count, accessor))
    }

    /// An accessor's values as floats, flattened; its type must have one of `allowed` components.
    fn floats(&self, index: usize, allowed: &[usize]) -> Result<Vec<f32>, MediaError> {
        let components = self.components(index)?;
        if !allowed.contains(&components) {
            return Err(corrupt("a vertex attribute has the wrong type"));
        }
        let (bytes, stride, _, count, accessor) = self.elements(index, components)?;
        let size = component_size(accessor.component_type).unwrap_or(4);
        self.charge(count.saturating_mul(components))?;
        let mut out = Vec::with_capacity(count.saturating_mul(components));
        for i in 0..count {
            for c in 0..components {
                let at = i * stride + c * size;
                let raw = bytes.get(at..at + size).ok_or_else(|| corrupt("an accessor runs past its buffer view"))?;
                out.push(component(raw, accessor.component_type, accessor.normalized));
            }
        }
        Ok(out)
    }

    /// Charges `values` decoded accessor values to the file's budget: enough for every vertex to
    /// carry a position, normal, colour and UV set, and for every triangle's indices, twice over.
    fn charge(&self, values: usize) -> Result<(), MediaError> {
        let budget = self.limits.max_vertices.saturating_mul(24).saturating_add(self.limits.max_triangles.saturating_mul(6));
        let total = self.decoded.get().saturating_add(values);
        if total > budget {
            return Err(MediaError::LimitExceeded("the glTF reads more vertex data than a model this size needs".into()));
        }
        self.decoded.set(total);
        Ok(())
    }

    fn indices(&self, index: usize) -> Result<Vec<u32>, MediaError> {
        if self.components(index)? != 1 {
            return Err(corrupt("triangle indices must be scalars"));
        }
        let (bytes, stride, size, count, accessor) = self.elements(index, 1)?;
        if !matches!(accessor.component_type, 5121 | 5123 | 5125) {
            return Err(corrupt("triangle indices must be unsigned integers"));
        }
        self.charge(count)?;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let raw = bytes.get(i * stride..i * stride + size).ok_or_else(|| corrupt("an accessor runs past its buffer view"))?;
            out.push(match raw {
                [a] => u32::from(*a),
                [a, b] => u32::from(u16::from_le_bytes([*a, *b])),
                [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]),
                _ => return Err(corrupt("an index has the wrong size")),
            });
        }
        Ok(out)
    }
}

/// Separators outside strings, and objects: cheap upper bounds on the values a JSON document
/// holds and on the structs it parses into.
fn json_values(json: &[u8]) -> (usize, usize) {
    let (mut count, mut objects, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for &b in json {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'{' => {
                    count += 1;
                    objects += 1;
                }
                b',' | b'[' | b':' => count += 1,
                _ => {}
            }
        }
    }
    (count, objects)
}

fn component_size(kind: u32) -> Option<usize> {
    match kind {
        5120 | 5121 => Some(1),
        5122 | 5123 => Some(2),
        5125 | 5126 => Some(4),
        _ => None,
    }
}

fn component(raw: &[u8], kind: u32, normalized: bool) -> f32 {
    let value = match (kind, raw) {
        (5120, [a]) => {
            let v = f32::from(*a as i8);
            if normalized { (v / 127.0).max(-1.0) } else { v }
        }
        (5121, [a]) => {
            let v = f32::from(*a);
            if normalized { v / 255.0 } else { v }
        }
        (5122, [a, b]) => {
            let v = f32::from(i16::from_le_bytes([*a, *b]));
            if normalized { (v / 32767.0).max(-1.0) } else { v }
        }
        (5123, [a, b]) => {
            let v = f32::from(u16::from_le_bytes([*a, *b]));
            if normalized { v / 65535.0 } else { v }
        }
        (5125, [a, b, c, d]) => u32::from_le_bytes([*a, *b, *c, *d]) as f32,
        (5126, [a, b, c, d]) => f32::from_le_bytes([*a, *b, *c, *d]),
        _ => 0.0,
    };
    if value.is_finite() { value } else { f32::NAN }
}

/// Triangle list indices for a primitive drawn in `mode` (4 list, 5 strip, 6 fan).
pub(super) fn triangulate(order: &[u32], mode: u32) -> Vec<u32> {
    match mode {
        5 => order
            .windows(3)
            .enumerate()
            .filter_map(|(i, w)| match *w {
                [a, b, c] => Some(if i % 2 == 0 { [a, b, c] } else { [b, a, c] }),
                _ => None,
            })
            .flatten()
            .collect(),
        6 => match order.split_first() {
            Some((&first, rest)) => rest
                .windows(2)
                .filter_map(|w| match *w {
                    [b, c] => Some([first, b, c]),
                    _ => None,
                })
                .flatten()
                .collect(),
            None => Vec::new(),
        },
        _ => order.get(..order.len() - order.len() % 3).unwrap_or_default().to_vec(),
    }
}

/// Decodes a `data:` URI's base64 payload; `None` for any other URI (an external file).
fn data_uri(uri: &str, max: usize) -> Result<Option<Vec<u8>>, MediaError> {
    let Some(rest) = uri.strip_prefix("data:") else {
        return Ok(None);
    };
    let (header, payload) = rest.split_once(',').ok_or_else(|| corrupt("a data URI has no data"))?;
    if !header.ends_with(";base64") {
        return Err(MediaError::UnsupportedFeature("only base64 data URIs can be read".into()));
    }
    if payload.len() / 4 * 3 > max {
        return Err(MediaError::TooLarge { size: payload.len() as u64, limit: max as u64 });
    }
    base64(payload.as_bytes()).map(Some).ok_or_else(|| corrupt("a data URI isn't valid base64"))
}

pub(super) fn base64(input: &[u8]) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        }))
    };
    let trimmed = input.strip_suffix(b"==").or_else(|| input.strip_suffix(b"=")).unwrap_or(input);
    let mut out = Vec::with_capacity(trimmed.len() / 4 * 3 + 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in trimmed {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn corrupt(why: &str) -> MediaError {
    MediaError::Decode(format!("the glTF file is damaged: {why}"))
}

fn too_many() -> MediaError {
    MediaError::LimitExceeded("an accessor has more elements than NodeCraft reads".into())
}

fn triple(values: &[f32], i: usize) -> [f32; 3] {
    [values.get(i * 3).copied().unwrap_or(0.0), values.get(i * 3 + 1).copied().unwrap_or(0.0), values.get(i * 3 + 2).copied().unwrap_or(0.0)]
}

fn modulate(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

/// The texel nearest `(u, v)`, repeating outside 0..1, as linear RGBA.
fn sample(texture: &Rgba, u: f32, v: f32) -> [f32; 4] {
    let wrap = |t: f32| if t.is_finite() { t - t.floor() } else { 0.0 };
    let x = ((wrap(u) * texture.width as f32) as u32).min(texture.width.saturating_sub(1));
    let y = ((wrap(v) * texture.height as f32) as u32).min(texture.height.saturating_sub(1));
    let at = (y as usize * texture.width as usize + x as usize) * 4;
    match texture.pixels.get(at..at + 4) {
        Some([r, g, b, a]) => [srgb_to_linear(*r), srgb_to_linear(*g), srgb_to_linear(*b), f32::from(*a) / 255.0],
        _ => WHITE,
    }
}

fn srgb_to_linear(c: u8) -> f32 {
    let v = f32::from(c) / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// A node's local transform: its matrix, or translation × rotation × scale (column-major).
fn local(node: &Node) -> [f32; 16] {
    let finite = |m: &[f32]| m.iter().all(|v| v.is_finite());
    if let Some(m) = node.matrix.filter(|m| finite(m)) {
        return m;
    }
    let t = node.translation.filter(|v| finite(v)).unwrap_or([0.0; 3]);
    let [x, y, z, w] = node.rotation.filter(|v| finite(v)).unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let len = (x * x + y * y + z * z + w * w).sqrt();
    let (x, y, z, w) = if len > 1e-12 { (x / len, y / len, z / len, w / len) } else { (0.0, 0.0, 0.0, 1.0) };
    let s = node.scale.filter(|v| finite(v)).unwrap_or([1.0; 3]);
    [
        (1.0 - 2.0 * (y * y + z * z)) * s[0],
        (2.0 * (x * y + z * w)) * s[0],
        (2.0 * (x * z - y * w)) * s[0],
        0.0,
        (2.0 * (x * y - z * w)) * s[1],
        (1.0 - 2.0 * (x * x + z * z)) * s[1],
        (2.0 * (y * z + x * w)) * s[1],
        0.0,
        (2.0 * (x * z + y * w)) * s[2],
        (2.0 * (y * z - x * w)) * s[2],
        (1.0 - 2.0 * (x * x + y * y)) * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}

fn point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    [m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12], m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13], m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14]]
}

fn det3(m: &[f32; 16]) -> f32 {
    m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2]) + m[8] * (m[1] * m[6] - m[5] * m[2])
}

/// The cofactor matrix of the upper 3×3, column-major: the inverse transpose up to scale, which
/// is all a normal needs before it is normalised.
fn normal_matrix(m: &[f32; 16]) -> [f32; 9] {
    let (a, b, c) = ([m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]);
    let (x, y, z) = (super::cross(b, c), super::cross(c, a), super::cross(a, b));
    [x[0], x[1], x[2], y[0], y[1], y[2], z[0], z[1], z[2]]
}

fn apply3(m: &[f32; 9], v: [f32; 3]) -> [f32; 3] {
    [m[0] * v[0] + m[3] * v[1] + m[6] * v[2], m[1] * v[0] + m[4] * v[1] + m[7] * v[2], m[2] * v[0] + m[5] * v[1] + m[8] * v[2]]
}

#[cfg(test)]
pub(super) mod test_support {
    pub(in crate::mesh) use super::{base64, triangulate};
}
