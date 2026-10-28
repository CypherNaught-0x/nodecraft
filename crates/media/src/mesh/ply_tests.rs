//! Independently authored ASCII bytes and adversarial admission cases, not vendor fixtures.
use super::*;
const FIXTURE: &[u8] = include_bytes!("fixtures/original-rgba.ply");
fn triangle() -> Mesh {
    Mesh {
        positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
        normals: vec![[0., 0., 2.]; 3],
        colors: vec![[1., 0., 0., 0.], [0., 0.5, 1., 0.5], [0.25, 1., 0.0031308, 0.25]],
        indices: vec![0, 1, 2],
    }
}
fn decode(b: &[u8]) -> Result<Mesh, crate::MediaError> {
    decode_mesh(b, &MeshLimits::default())
}
#[test]
fn ply1011_independent_layout_and_channel_precision_are_exact() {
    let bytes = encode_ply(&triangle(), FIXTURE.len()).unwrap();
    assert_eq!(bytes, FIXTURE);
    assert_eq!(crate::sniff(FIXTURE).unwrap().mime, "model/ply");
    assert_eq!(MeshFormat::from_mime("model/ply"), Some(MeshFormat::Ply));
    assert_eq!(MeshFormat::Ply.extension(), "ply");
    let mesh = decode(FIXTURE).unwrap();
    assert_eq!(mesh.positions, triangle().positions);
    assert_eq!(mesh.indices, [0, 1, 2]);
    assert_eq!(mesh.normals, vec![[0., 0., 1.]; 3]);
    assert_eq!(mesh.colors[0], [1., 0., 0., 0.], "transparent red remains unassociated RGB, never discarded or premultiplied");
    assert!((mesh.colors[1][1] - 0.5028865).abs() < 0.000001);
    assert!((mesh.colors[2][0] - 0.2501583).abs() < 0.000001);
    assert!((mesh.colors[2][2] - 0.00303527).abs() < 0.0000001);
    assert_eq!(mesh.colors[1][3], 128.0 / 255.0);
    assert_eq!(mesh.colors[2][3], 64.0 / 255.0);
    assert_eq!(encode_ply(&mesh, FIXTURE.len()).unwrap(), FIXTURE);
    let glb = decode(&encode_glb(&mesh).unwrap()).unwrap();
    assert_eq!(glb.colors, mesh.colors, "GLB stores RGBA data; its opaque material behavior is unchanged");
    let obj = decode(&encode_obj(&mesh).unwrap()).unwrap();
    assert_eq!(obj.positions, mesh.positions);
    assert_eq!(obj.indices, mesh.indices);
    assert!(obj.colors.iter().all(|c| c[3] == 1.0), "existing OBJ has no alpha channel");
}
#[test]
fn ply1011_exact_input_output_and_decoded_limits_precede_owned_allocations() {
    let exact = MeshLimits { max_input_bytes: FIXTURE.len(), max_vertices: 3, max_triangles: 1, ..Default::default() };
    assert!(decode_mesh(FIXTURE, &exact).is_ok());
    for limits in
        [MeshLimits { max_input_bytes: FIXTURE.len() - 1, ..exact }, MeshLimits { max_vertices: 2, ..exact }, MeshLimits { max_triangles: 0, ..exact }]
    {
        ply::reset_allocations();
        assert!(decode_mesh(FIXTURE, &limits).is_err());
        assert_eq!(ply::allocations(), [0, 0]);
    }
    ply::reset_allocations();
    assert!(encode_ply(&triangle(), FIXTURE.len() - 1).is_err());
    assert_eq!(ply::allocations(), [0, 0]);
    assert!(encode_ply(&triangle(), FIXTURE.len()).is_ok());
    let mut source = triangle();
    source.positions = vec![[f32::MAX; 3]; 120_000];
    source.normals = vec![[0., 0., 1.]; 120_000];
    source.colors = vec![[1.; 4]; 120_000];
    ply::reset_allocations();
    assert!(encode_ply(&source, 16 * 1024 * 1024).is_err());
    assert_eq!(ply::allocations(), [0, 0], "action result cap is checked by formatting into a counter first");
}
#[test]
fn ply1011_supported_line_endings_comments_aliases_and_normal_normalization() {
    let text = std::str::from_utf8(FIXTURE).unwrap();
    for ending in ["\n", "\r\n", "\r"] {
        let text = text
            .replace("format ascii 1.0\n", "format ascii 1.0\ncomment original profile test\n")
            .replace("float ", "float32 ")
            .replace("uchar ", "uint8 ")
            .replace("uint8 int vertex_indices", "uint8 int32 vertex_index")
            .replace(" 0 0 1 ", " 0 0 2 ")
            .replace('\n', ending);
        let m = decode(text.as_bytes()).unwrap();
        assert_eq!(m.normals, vec![[0., 0., 1.]; 3]);
    }
    let no_final = &FIXTURE[..FIXTURE.len() - 1];
    assert!(decode(no_final).is_ok());
    for n in [[f32::MIN_POSITIVE, 0., 0.], [f32::MAX, 0., 0.]] {
        let mut m = triangle();
        m.normals = vec![n; 3];
        let m = decode(&encode_ply(&m, 4096).unwrap()).unwrap();
        assert_eq!(m.normals, vec![[1., 0., 0.]; 3]);
    }
}
#[test]
fn ply1011_complete_body_and_all_header_fields_refuse_before_geometry_allocation() {
    let text = std::str::from_utf8(FIXTURE).unwrap();
    let mut invalid = vec![];
    for (a, b) in [
        ("ascii 1.0", "binary_little_endian 1.0"),
        ("ascii 1.0", "ascii 2.0"),
        ("vertex 3", "vertex 4000001"),
        ("face 1", "face 999999999999999999999999"),
        ("vertex 3", "vertex 0"),
        ("float nx", "float u"),
        ("uchar alpha", "float alpha"),
        ("float x", "double x"),
        ("property list uchar int vertex_indices", "property list uchar uint vertex_indices"),
        ("end_header", "element edge 1\nend_header"),
        ("3 0 1 2", "4 0 1 2 0"),
        ("3 0 1 2", "3 0 1 3"),
        ("3 0 1 2", "3 0 1 -1"),
        ("3 0 1 2", "3 0 1 4294967295"),
        ("3 0 1 2", "3 0 1 2 extra"),
        ("255 0 0 0", "256 0 0 0"),
        ("255 0 0 0", "255.0 0 0 0"),
        ("255 0 0 0", "255 0 0 -1"),
        ("0 0 0 0 0 1", "NaN 0 0 0 0 1"),
        ("0 0 0 0 0 1", "inf 0 0 0 0 1"),
        ("0 0 0 0 0 1", "0 0 0 0 0 0"),
        ("0 0 0 0 0 1", "1e-99 0 0 0 0 1"),
        ("0 0 0 0 0 1", "1e-999 0 0 0 0 1"),
    ] {
        invalid.push(text.replacen(a, b, 1).into_bytes());
    }
    invalid.push([FIXTURE, b"\n"].concat());
    invalid.push([FIXTURE, b"3 0 1 2\n"].concat());
    invalid.push(text.replace("format ascii 1.0\n", &format!("format ascii 1.0\n{}", "comment x\n".repeat(65))).into_bytes());
    invalid.push(text.replace("format ascii 1.0\n", &format!("format ascii 1.0\n{}", format!("comment {}\n", "x".repeat(1000)).repeat(17))).into_bytes());
    invalid.push(text.replace("0 0 0 0 0 1", &format!("{} 0 0 0 0 1", "1".repeat(65))).into_bytes());
    invalid.push(text.replace("0 0 0 0 0 1", &format!("{} 0 0 0 0 1", "1".repeat(1025))).into_bytes());
    invalid.push(text.replace("end_header", "end_header\0").into_bytes());
    for bytes in invalid {
        ply::reset_allocations();
        assert!(decode(&bytes).is_err(), "{}", String::from_utf8_lossy(&bytes));
        assert_eq!(ply::allocations(), [0, 0]);
    }
    for len in 0..FIXTURE.len() - 1 {
        ply::reset_allocations();
        assert!(decode_mesh_as(&FIXTURE[..len], MeshFormat::Ply, &Default::default()).is_err(), "{len}");
        assert_eq!(ply::allocations(), [0, 0]);
    }
}
#[test]
fn ply1011_writer_rejects_invalid_attributes_without_allocating_output() {
    for kind in 0..7 {
        let mut m = triangle();
        match kind {
            0 => m.positions[2][0] = f32::NAN,
            1 => m.normals[2] = [0.; 3],
            2 => m.normals[2][0] = f32::INFINITY,
            3 => m.colors[2][3] = f32::NAN,
            4 => m.colors[2][0] = -0.1,
            5 => m.colors[2][3] = 1.1,
            _ => m.indices[2] = 3,
        };
        ply::reset_allocations();
        assert!(encode_ply(&m, 4096).is_err());
        assert_eq!(ply::allocations(), [0, 0]);
    }
    assert!(encode_ply(&Mesh::default(), 4096).is_err());
}
#[test]
fn ply1011_recognition_keeps_arbitrary_binary_stl_headers_and_plain_text_behavior() {
    let mut stl = encode_stl(&triangle(), 134).unwrap();
    stl[..4].copy_from_slice(b"ply\n");
    assert_eq!(crate::sniff(&stl).unwrap().mime, "model/stl");
    assert!(decode(&stl).is_ok());
    assert_ne!(crate::sniff(b"plywood furniture is made of layers").map(|s| s.mime), Some("model/ply"));
}

#[test]
fn ply1011_all_rgba_byte_values_survive_decode_encode_without_premultiplication() {
    let mut m = triangle();
    m.positions = vec![[0., 0., 0.]; 256];
    m.normals = vec![[0., 0., 1.]; 256];
    m.colors = (0u8..=255).map(|c| [f32::from(c) / 255.; 4]).collect();
    let encoded = encode_ply(&m, 65536).unwrap();
    let decoded = decode(&encoded).unwrap();
    assert_eq!(decoded.colors.iter().map(|c| c[3]).collect::<Vec<_>>(), m.colors.iter().map(|c| c[3]).collect::<Vec<_>>());
    assert_eq!(encode_ply(&decoded, 65536).unwrap(), encoded);
}

#[test]
fn ply1011_exact_header_line_and_token_caps_are_admitted_and_one_over_refused() {
    let text = std::str::from_utf8(FIXTURE).unwrap();
    let base_lines = text.split("end_header\n").next().unwrap().lines().count() + 1;
    let with_lines = |n: usize| text.replace("format ascii 1.0\n", &format!("format ascii 1.0\n{}", "comment x\n".repeat(n - base_lines)));
    assert!(decode(with_lines(64).as_bytes()).is_ok());
    assert!(decode(with_lines(65).as_bytes()).is_err());
    let base_bytes = text.find("end_header\n").unwrap() + "end_header\n".len();
    let with_bytes = |n: usize| {
        let mut remaining = n - base_bytes;
        let mut comments = String::new();
        while remaining > 1025 {
            comments.push_str(&format!("comment {}\n", "x".repeat(1016)));
            remaining -= 1025;
        }
        comments.push_str(&format!("comment {}\n", "x".repeat(remaining - 9)));
        text.replace("format ascii 1.0\n", &format!("format ascii 1.0\n{comments}"))
    };
    assert!(decode(with_bytes(16384).as_bytes()).is_ok());
    assert!(decode(with_bytes(16385).as_bytes()).is_err());
    assert!(decode(text.replace("format ascii 1.0\n", &format!("format ascii 1.0\ncomment {}\n", "x".repeat(1016))).as_bytes()).is_ok());
    assert!(decode(text.replace("format ascii 1.0\n", &format!("format ascii 1.0\ncomment {}\n", "x".repeat(1017))).as_bytes()).is_err());
    assert!(decode(text.replace("0 0 0 0 0 1", &format!("0.{} 0 0 0 0 1", "0".repeat(62))).as_bytes()).is_ok());
    assert!(decode(text.replace("0 0 0 0 0 1", &format!("0.{} 0 0 0 0 1", "0".repeat(63))).as_bytes()).is_err());
}
