use super::*;
use crate::{MediaKind, sniff};
use serde_json::json;

/// A unit cube, eight shared corners and twelve triangles, coloured red.
fn cube() -> Mesh {
    let positions: Vec<[f32; 3]> = (0..8).map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32]).collect();
    let indices = vec![0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4, 2, 6, 3, 3, 6, 7, 0, 4, 2, 2, 4, 6, 1, 3, 5, 3, 7, 5];
    let mut mesh = Mesh { normals: vec![[0.0; 3]; 8], colors: vec![[1.0, 0.0, 0.0, 1.0]; 8], positions, indices };
    repair_normals(&mut mesh);
    mesh
}

fn close<const N: usize>(a: &[[f32; N]], b: &[[f32; N]], tolerance: f32) -> bool {
    a.len() == b.len() && a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() <= tolerance)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A one-triangle glTF JSON with its buffer embedded, and room to change the document.
fn triangle_gltf(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    triangle_gltf_with(|_| {}, edit)
}

/// [`triangle_gltf`], also changing the embedded buffer (three VEC3 floats, then u16 indices).
fn triangle_gltf_with(bin_edit: impl FnOnce(&mut Vec<u8>), edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut bin = Vec::new();
    for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    for i in [0u16, 1, 2, 0] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    bin_edit(&mut bin);
    let mut doc = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}
        ],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 6}],
        "buffers": [{"byteLength": bin.len(), "uri": format!("data:application/octet-stream;base64,{}", base64(&bin))}]
    });
    edit(&mut doc);
    serde_json::to_vec(&doc).unwrap()
}

#[test]
fn glb_round_trips_geometry_normals_and_colours() {
    let mesh = cube();
    let glb = encode_glb(&mesh).unwrap();
    assert_eq!(sniff(&glb).map(|s| (s.mime, s.kind)), Some(("model/gltf-binary", MediaKind::Model3d)));
    let back = decode_mesh(&glb, &MeshLimits::default()).unwrap();
    assert_eq!((&back.positions, &back.colors, &back.indices), (&mesh.positions, &mesh.colors, &mesh.indices));
    assert!(close(&back.normals, &mesh.normals, 1e-6));
    assert_eq!(back.info().triangles, 12);
    let bounds = back.bounds().unwrap();
    assert_eq!((bounds.min, bounds.max), ([0.0; 3], [1.0; 3]));
    assert!((bounds.radius() - 3f32.sqrt() / 2.0).abs() < 1e-6);
}

#[test]
fn obj_round_trips_and_reads_common_corner_forms() {
    let mesh = cube();
    let obj = encode_obj(&mesh).unwrap();
    assert_eq!(sniff(&obj).map(|s| s.mime), Some("model/obj"));
    let back = decode_mesh(&obj, &MeshLimits::default()).unwrap();
    // Vertices are renumbered in the order faces first use them; the triangles are the same.
    let corners = |m: &Mesh| m.indices.iter().map(|&i| m.positions[i as usize]).collect::<Vec<_>>();
    assert_eq!(corners(&back), corners(&mesh));
    assert!(back.colors.iter().all(|c| close(&[*c], &[[1.0, 0.0, 0.0, 1.0]], 0.01)));

    let quad = b"# a quad\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvn 0 0 1\nf 1/1/1 2/1/1 3/1/1 -1/1/-1\n";
    let mesh = decode_mesh(quad, &MeshLimits::default()).unwrap();
    assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3], "a quad fans into two triangles, sharing corners");
    assert!(mesh.normals.iter().all(|n| *n == [0.0, 0.0, 1.0]));
    let plain = decode_mesh_as(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", MeshFormat::Obj, &MeshLimits::default()).unwrap();
    assert_eq!(plain.normals, vec![[0.0, 0.0, 1.0]; 3], "missing normals are computed from the faces");
}

#[test]
fn gltf_json_with_embedded_data_reads_and_applies_node_transforms() {
    let plain = triangle_gltf(|_| {});
    assert_eq!(sniff(&plain).map(|s| s.mime), Some("model/gltf+json"));
    let mesh = decode_mesh(&plain, &MeshLimits::default()).unwrap();
    assert_eq!(mesh.positions, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    assert_eq!(mesh.normals, vec![[0.0, 0.0, 1.0]; 3]);
    assert_eq!(mesh.colors, vec![[1.0; 4]; 3], "no material is white");

    // A parent translation, a child scale of -1 on x (mirroring flips the winding back) and a
    // red material.
    let moved = triangle_gltf(|doc| {
        doc["nodes"] = json!([{"translation": [10.0, 0.0, 0.0], "children": [1]}, {"mesh": 0, "scale": [-1.0, 1.0, 1.0]}]);
        doc["materials"] = json!([{"pbrMetallicRoughness": {"baseColorFactor": [1.0, 0.0, 0.0, 1.0]}}]);
        doc["meshes"][0]["primitives"][0]["material"] = json!(0);
    });
    let mesh = decode_mesh(&moved, &MeshLimits::default()).unwrap();
    assert_eq!(mesh.positions, vec![[10.0, 0.0, 0.0], [9.0, 0.0, 0.0], [10.0, 1.0, 0.0]]);
    assert_eq!(mesh.indices, vec![0, 2, 1], "a mirrored node keeps its triangles facing out");
    assert_eq!(mesh.normals, vec![[0.0, 0.0, 1.0]; 3]);
    assert_eq!(mesh.colors, vec![[1.0, 0.0, 0.0, 1.0]; 3]);

    let quarter = triangle_gltf(|doc| doc["nodes"][0]["rotation"] = json!([0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2]));
    let mesh = decode_mesh(&quarter, &MeshLimits::default()).unwrap();
    let p = mesh.positions[1];
    assert!(p[0].abs() < 1e-6 && (p[1] - 1.0).abs() < 1e-6, "a quarter turn about z moves +x to +y: {p:?}");
}

#[test]
fn gltf_textures_become_vertex_colours() {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let textured = triangle_gltf(|doc| {
        let mut uv = Vec::new();
        for v in [0.25f32, 0.25, 0.75, 0.25, 0.25, 0.75] {
            uv.extend_from_slice(&v.to_le_bytes());
        }
        doc["buffers"].as_array_mut().unwrap().push(json!({"byteLength": uv.len(), "uri": format!("data:application/octet-stream;base64,{}", base64(&uv))}));
        doc["bufferViews"].as_array_mut().unwrap().push(json!({"buffer": 1, "byteLength": uv.len()}));
        doc["accessors"].as_array_mut().unwrap().push(json!({"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"}));
        doc["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"] = json!(2);
        doc["meshes"][0]["primitives"][0]["material"] = json!(0);
        doc["materials"] = json!([{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}]);
        doc["textures"] = json!([{"source": 0}]);
        doc["images"] = json!([{"uri": format!("data:image/png;base64,{}", base64(&png))}]);
    });
    let mesh = decode_mesh(&textured, &MeshLimits::default()).unwrap();
    assert_eq!(mesh.colors, vec![[1.0, 0.0, 0.0, 1.0]; 3]);

    // A texture past the limit is left out; the mesh still reads.
    let limits = MeshLimits { max_texture_pixels: 1, ..MeshLimits::default() };
    assert_eq!(decode_mesh(&textured, &limits).unwrap().colors, vec![[1.0; 4]; 3]);
}

#[test]
fn strips_fans_and_unindexed_primitives_triangulate() {
    assert_eq!(gltf::test_support::triangulate(&[0, 1, 2, 3], 5), vec![0, 1, 2, 2, 1, 3]);
    assert_eq!(gltf::test_support::triangulate(&[0, 1, 2, 3], 6), vec![0, 1, 2, 0, 2, 3]);
    assert_eq!(gltf::test_support::triangulate(&[0, 1, 2, 3], 4), vec![0, 1, 2], "a partial triangle is dropped");
    let unindexed = triangle_gltf(|doc| {
        doc["meshes"][0]["primitives"][0].as_object_mut().unwrap().remove("indices");
    });
    assert_eq!(decode_mesh(&unindexed, &MeshLimits::default()).unwrap().indices, vec![0, 1, 2]);
    let points = triangle_gltf(|doc| doc["meshes"][0]["primitives"][0]["mode"] = json!(0));
    assert!(matches!(decode_mesh(&points, &MeshLimits::default()), Err(MediaError::Decode(_))), "points alone have nothing to show");
}

#[test]
fn hostile_gltf_is_refused_with_a_reason_never_a_panic() {
    let limits = MeshLimits::default();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("external buffer", triangle_gltf(|doc| doc["buffers"][0]["uri"] = json!("../../secret.bin"))),
        ("draco", triangle_gltf(|doc| doc["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]))),
        ("glTF 1", triangle_gltf(|doc| doc["asset"]["version"] = json!("1.0"))),
        ("accessor past view", triangle_gltf(|doc| doc["accessors"][0]["count"] = json!(4))),
        ("huge count", triangle_gltf(|doc| doc["accessors"][0]["count"] = json!(u64::MAX))),
        ("huge offset", triangle_gltf(|doc| doc["accessors"][0]["byteOffset"] = json!(u64::MAX))),
        ("view past buffer", triangle_gltf(|doc| doc["bufferViews"][0]["byteLength"] = json!(1000))),
        ("bad stride", triangle_gltf(|doc| doc["bufferViews"][0]["byteStride"] = json!(2))),
        ("index out of range", triangle_gltf_with(|bin| bin.splice(40..42, 7u16.to_le_bytes()).for_each(drop), |_| {})),
        ("missing accessor", triangle_gltf(|doc| doc["meshes"][0]["primitives"][0]["attributes"]["POSITION"] = json!(99))),
        ("missing mesh", triangle_gltf(|doc| doc["nodes"][0]["mesh"] = json!(7))),
        ("missing child", triangle_gltf(|doc| doc["nodes"][0]["children"] = json!([42]))),
        ("cycle", triangle_gltf(|doc| doc["nodes"] = json!([{"mesh": 0, "children": [1]}, {"children": [0]}]))),
        ("self child", triangle_gltf(|doc| doc["nodes"][0]["children"] = json!([0]))),
        ("sparse", triangle_gltf(|doc| doc["accessors"][0]["sparse"] = json!({"count": 1}))),
        ("matrix attribute", triangle_gltf(|doc| doc["accessors"][0]["type"] = json!("MAT4"))),
        ("float indices", triangle_gltf(|doc| doc["accessors"][1]["componentType"] = json!(5126))),
        ("short data uri", triangle_gltf(|doc| doc["buffers"][0]["byteLength"] = json!(10_000))),
        ("bad base64", triangle_gltf(|doc| doc["buffers"][0]["uri"] = json!("data:application/octet-stream;base64,@@@@"))),
        ("not base64", triangle_gltf(|doc| doc["buffers"][0]["uri"] = json!("data:text/plain,hello"))),
        ("nan position", triangle_gltf_with(|bin| bin.splice(0..4, f32::NAN.to_le_bytes()).for_each(drop), |_| {})),
        ("not json", b"{\"asset\": {\"version\": \"2.0\"}, \"scenes\": [".to_vec()),
    ];
    for (name, bytes) in cases {
        let result = decode_mesh_as(&bytes, MeshFormat::Gltf, &limits);
        assert!(result.is_err(), "{name} was accepted");
        let message = result.unwrap_err().to_string();
        assert!(!message.is_empty(), "{name}");
        if name == "external buffer" {
            assert!(message.contains(".glb"), "{message}");
            assert!(!message.contains("secret"), "the file's own path isn't echoed: {message}");
        }
        if name == "draco" {
            assert!(message.contains("KHR_draco_mesh_compression"), "{message}");
        }
    }
}

#[test]
fn hostile_glb_headers_are_refused() {
    let limits = MeshLimits::default();
    let good = encode_glb(&cube()).unwrap();
    let mut cases: Vec<Vec<u8>> =
        vec![b"glTF".to_vec(), b"glTF\x01\0\0\0\x0c\0\0\0".to_vec(), b"glTF\x02\0\0\0\xff\xff\xff\xff".to_vec(), b"glTF\x02\0\0\0\x0c\0\0\0".to_vec()];
    let mut long_chunk = good.clone();
    long_chunk[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(long_chunk);
    let mut wrong_kind = good.clone();
    wrong_kind[16..20].copy_from_slice(&0x004E_4942u32.to_le_bytes());
    cases.push(wrong_kind);
    for len in 0..good.len().min(200) {
        cases.push(good[..len].to_vec());
    }
    let mut no_bin = good.clone();
    let json_len = u32::from_le_bytes(good[12..16].try_into().unwrap()) as usize;
    no_bin.truncate(20 + json_len);
    let total = (no_bin.len() as u32).to_le_bytes();
    no_bin[8..12].copy_from_slice(&total);
    cases.push(no_bin);
    for bytes in cases {
        assert!(decode_mesh_as(&bytes, MeshFormat::Glb, &limits).is_err(), "{} bytes", bytes.len());
    }
}

#[test]
fn hostile_obj_is_refused_with_a_line_number() {
    let limits = MeshLimits::default();
    for (bytes, needle) in [
        (b"v 0 0 0\nf 1 2 3\n".as_slice(), "line 2"),
        (b"v 0 0\n", "line 1"),
        (b"v 0 0 nan\n", "line 1"),
        (b"v 0 0 0\nv 1 0 0\nf 1 2\n", "line 3"),
        (b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 0 1 2\n", "line 4"),
        (b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf -4 1 2\n", "line 4"),
        (b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1//9 2 3\n", "line 4"),
        (b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1/2/3/4 2 3\n", "line 4"),
        (b"v 1 2 3 4 5 6 7 8 9\n", "line 1"),
        (b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 99999999999999999999 2 3\n", "line 4"),
    ] {
        let message = decode_mesh_as(bytes, MeshFormat::Obj, &limits).unwrap_err().to_string();
        assert!(message.contains(needle), "{message}");
    }
    assert!(decode_mesh_as(b"# nothing\n", MeshFormat::Obj, &limits).is_err(), "no triangles");
    assert!(decode_mesh_as(b"v 0 0 0\n\xff\xfe\n", MeshFormat::Obj, &limits).is_err());
    assert!(decode_mesh_as(b"# caf\xe9 comment in Latin-1\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", MeshFormat::Obj, &limits).is_ok(), "comments aren't decoded");
}

#[test]
fn limits_are_enforced_before_allocation() {
    let tight = MeshLimits { max_vertices: 3, max_triangles: 1, ..MeshLimits::default() };
    assert!(decode_mesh_as(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", MeshFormat::Obj, &tight).is_ok());
    let four = decode_mesh_as(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nf 1 2 3\n", MeshFormat::Obj, &tight);
    assert!(matches!(four, Err(MediaError::LimitExceeded(_))));
    let two = decode_mesh_as(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nf 1 3 2\n", MeshFormat::Obj, &tight);
    assert!(matches!(two, Err(MediaError::LimitExceeded(_))));
    assert!(matches!(decode_mesh(&encode_glb(&cube()).unwrap(), &tight), Err(MediaError::LimitExceeded(_))));
    let small = MeshLimits { max_input_bytes: 10, ..MeshLimits::default() };
    assert!(matches!(decode_mesh(&encode_glb(&cube()).unwrap(), &small), Err(MediaError::TooLarge { .. })));
    let shallow = MeshLimits { max_depth: 1, ..MeshLimits::default() };
    let deep = triangle_gltf(|doc| doc["nodes"] = json!([{"children": [1]}, {"children": [2]}, {"mesh": 0}]));
    assert!(matches!(decode_mesh(&deep, &shallow), Err(MediaError::LimitExceeded(_))));
    assert!(decode_mesh(&deep, &MeshLimits::default()).is_ok());
    let wide = MeshLimits { max_node_visits: 3, ..MeshLimits::default() };
    let shared = triangle_gltf(|doc| doc["nodes"] = json!([{"children": [1, 1, 1]}, {"mesh": 0}]));
    assert!(matches!(decode_mesh(&shared, &wide), Err(MediaError::LimitExceeded(_))), "each instance is a visit");
    let mut values = b"{\"asset\":{\"version\":\"2.0\"},\"scenes\":[],\"x\":[".to_vec();
    values.extend(std::iter::repeat_n(b"0,".as_slice(), 4_000_001).flatten());
    values.extend_from_slice(b"0]}");
    assert!(matches!(decode_mesh_as(&values, MeshFormat::Gltf, &MeshLimits::default()), Err(MediaError::LimitExceeded(_))));
}

#[test]
fn edges_list_each_once_and_respect_their_cap() {
    let mesh = cube();
    let edges = mesh.edges(usize::MAX).unwrap();
    assert_eq!(edges.len() / 2, 18, "a triangulated cube has 12 sides and 6 diagonals");
    assert!(mesh.edges(17).is_none());
}

#[test]
fn writers_refuse_invalid_meshes() {
    let mut broken = cube();
    broken.indices.push(99);
    assert!(encode_glb(&broken).is_err());
    assert!(encode_obj(&broken).is_err());
    assert!(encode_glb(&Mesh::default()).is_err());
    assert!(encode_obj(&Mesh::default()).is_err());
    let mut nan = cube();
    nan.positions[0][0] = f32::NAN;
    assert!(encode_glb(&nan).is_err());
}

#[test]
fn base64_decodes_padded_unpadded_and_url_safe() {
    assert_eq!(gltf::test_support::base64(b"aGVsbG8=").unwrap(), b"hello");
    assert_eq!(gltf::test_support::base64(b"aGVsbG8").unwrap(), b"hello");
    assert_eq!(gltf::test_support::base64(b"-_8=").unwrap(), vec![0xfb, 0xff]);
    assert!(gltf::test_support::base64(b"a b").is_none());
}

#[test]
fn shared_primitives_and_rereads_are_bounded_work() {
    // One node lists another a hundred times, whose mesh has twenty primitives that draw nothing:
    // free per visit, so they count against the visit cap themselves.
    let mut limits = MeshLimits { max_node_visits: 1000, ..MeshLimits::default() };
    let empty = triangle_gltf(|doc| {
        doc["nodes"] = json!([{"children": vec![1; 100]}, {"mesh": 1}, {"mesh": 0}]);
        doc["scenes"] = json!([{"nodes": [0, 2]}]);
        doc["meshes"] = json!([doc["meshes"][0].clone(), {"primitives": vec![json!({"attributes": {}}); 20]}]);
    });
    let message = decode_mesh(&empty, &limits).unwrap_err().to_string();
    assert!(message.contains("primitives"), "{message}");

    // An unused NORMAL accessor is decoded again at every instance of an empty primitive.
    limits = MeshLimits { max_vertices: 10, max_triangles: 10, ..MeshLimits::default() };
    let reread = triangle_gltf(|doc| {
        doc["nodes"] = json!([{"children": vec![1; 100]}, {"mesh": 1}, {"mesh": 0}]);
        doc["scenes"] = json!([{"nodes": [0, 2]}]);
        doc["accessors"].as_array_mut().unwrap().push(json!({"bufferView": 0, "componentType": 5126, "count": 0, "type": "VEC3"}));
        doc["meshes"] = json!([doc["meshes"][0].clone(), {"primitives": [{"attributes": {"POSITION": 2, "NORMAL": 0}}]}]);
    });
    let message = decode_mesh(&reread, &limits).unwrap_err().to_string();
    assert!(message.contains("vertex data"), "{message}");
}

#[test]
fn many_small_json_objects_are_refused_before_parsing() {
    let crowded = triangle_gltf(|doc| doc["nodes"] = json!(vec![json!({}); 300_000]));
    let message = decode_mesh(&crowded, &MeshLimits::default()).unwrap_err().to_string();
    assert!(message.contains("too many values") && message.starts_with("media exceeds"), "{message}");
}

#[test]
fn mirrored_normals_stay_outward_and_colours_stay_finite() {
    // NORMAL (three +z) and COLOR_0 (NaN, 2, -1, 0.5) follow the triangle's data.
    let file = triangle_gltf_with(
        |bin| {
            for v in [0.0f32, 0.0, 1.0].repeat(3) {
                bin.extend_from_slice(&v.to_le_bytes());
            }
            for v in [f32::NAN, 2.0, -1.0, 0.5].repeat(3) {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        },
        |doc| {
            doc["nodes"] = json!([{"mesh": 0, "scale": [-1.0, 1.0, 1.0]}]);
            doc["meshes"][0]["primitives"][0]["attributes"] = json!({"POSITION": 0, "NORMAL": 2, "COLOR_0": 3});
            let views = doc["bufferViews"].as_array_mut().unwrap();
            views.push(json!({"buffer": 0, "byteOffset": 44, "byteLength": 36}));
            views.push(json!({"buffer": 0, "byteOffset": 80, "byteLength": 48}));
            let accessors = doc["accessors"].as_array_mut().unwrap();
            accessors.push(json!({"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC3"}));
            accessors.push(json!({"bufferView": 3, "componentType": 5126, "count": 3, "type": "VEC4"}));
        },
    );
    let mesh = decode_mesh(&file, &MeshLimits::default()).unwrap();
    assert_eq!(mesh.normals, vec![[0.0, 0.0, 1.0]; 3], "a mirror about x leaves a +z normal pointing +z");
    assert_eq!(mesh.colors, vec![[1.0, 1.0, 0.0, 0.5]; 3], "NaN becomes 1, others are clamped");
    assert!(encode_glb(&mesh).is_ok());
}

#[test]
fn a_required_texture_transform_is_refused_not_ignored() {
    let file = triangle_gltf(|doc| doc["extensionsRequired"] = json!(["KHR_texture_transform"]));
    let message = decode_mesh(&file, &MeshLimits::default()).unwrap_err().to_string();
    assert!(message.contains("KHR_texture_transform"), "{message}");
}
