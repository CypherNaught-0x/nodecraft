//! Original byte fixtures and independent layout checks; no vendor assets.
use super::*;

fn triangle() -> Mesh {
    Mesh {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: vec![[0.0, 1.0, 0.0]; 3],
        colors: vec![[0.2, 0.5, 0.9, 0.4]; 3],
        indices: vec![0, 1, 2],
    }
}

fn fixture() -> Vec<u8> {
    let mut out = vec![0; 80];
    out.extend_from_slice(&1u32.to_le_bytes());
    // Stored +Z facet normal, then the origin, +X, +Y vertices, followed by zero attributes.
    for n in [0.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        out.extend_from_slice(&n.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn mesh569_stl_writer_matches_independent_binary_layout_and_recomputes_flat_normals() {
    let mesh = triangle();
    let bytes = encode_stl(&mesh, 134).unwrap();
    assert_eq!(bytes.len(), 134);
    assert!(bytes.starts_with(b"NodeCraft binary STL"));
    assert_eq!(&bytes[80..], &fixture()[80..]);
    assert_eq!(crate::sniff(&bytes).unwrap().mime, "model/stl");
    assert_eq!(MeshFormat::from_mime("model/stl"), Some(MeshFormat::Stl));
    assert_eq!(MeshFormat::Stl.extension(), "stl");
    let decoded = decode_mesh(&bytes, &MeshLimits::default()).unwrap();
    assert_eq!(decoded.positions, mesh.positions);
    assert_eq!(decoded.indices, mesh.indices);
    assert_eq!(decoded.normals, vec![[0.0, 0.0, 1.0]; 3]);
    assert_eq!(decoded.colors, vec![[1.0; 4]; 3], "the defined STL profile has no colours");
    assert_eq!(mesh, triangle(), "the source geometry/attributes are retained");
}

#[test]
fn mesh569_stl_reads_non_nodecraft_headers_and_preserves_signed_model_coordinates() {
    for x in [-1000.0, -1.0, 0.0, 0.25, 999.0] {
        for scale in [1e-30f32, 0.5, 1.0, 1e30] {
            // Translation must not swallow tiny sides at f32 precision.
            let offset = if scale < 1e-10 { 0.0 } else { x };
            let mut mesh = triangle();
            mesh.positions = vec![[offset, 0.0, 0.0], [offset + scale, 0.0, 0.0], [offset, scale, 0.0]];
            let mut bytes = encode_stl(&mesh, 134).unwrap();
            bytes[..80].fill(0);
            bytes[..20].copy_from_slice(b"solid binary profile");
            let decoded = decode_mesh(&bytes, &MeshLimits::default()).unwrap();
            assert_eq!(decoded.positions, mesh.positions);
            assert_eq!(decoded.normals, vec![[0.0, 0.0, 1.0]; 3]);
        }
    }
}

#[test]
fn mesh569_stl_winding_controls_normals_and_shared_vertices_are_expanded() {
    let mut mesh = triangle();
    mesh.indices.extend([2, 1, 0]);
    let decoded = decode_mesh(&encode_stl(&mesh, 184).unwrap(), &MeshLimits::default()).unwrap();
    assert_eq!(decoded.vertex_count(), 6);
    assert_eq!(decoded.triangle_count(), 2);
    assert_eq!(&decoded.normals[..3], &[[0.0, 0.0, 1.0]; 3]);
    assert_eq!(&decoded.normals[3..], &[[0.0, 0.0, -1.0]; 3]);
    assert_eq!(decoded.indices, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn mesh569_stl_exact_byte_and_decoded_count_limits_are_admitted() {
    let bytes = fixture();
    let exact = MeshLimits { max_input_bytes: 134, max_vertices: 3, max_triangles: 1, ..Default::default() };
    assert!(decode_mesh(&bytes, &exact).is_ok());
    assert!(encode_stl(&triangle(), 134).is_ok());
    assert!(matches!(encode_stl(&triangle(), 133), Err(MediaError::LimitExceeded(_))));
    for limits in [MeshLimits { max_input_bytes: 133, ..exact }, MeshLimits { max_vertices: 2, ..exact }, MeshLimits { max_triangles: 0, ..exact }] {
        assert!(decode_mesh(&bytes, &limits).is_err());
    }
    let mut many = triangle();
    many.indices = [0, 1, 2].repeat(400_000);
    assert!(matches!(encode_stl(&many, 16 * 1024 * 1024), Err(MediaError::LimitExceeded(_))));
    many.indices = [0, 1, 2].repeat(1_333_334);
    assert!(matches!(encode_stl(&many, usize::MAX), Err(MediaError::LimitExceeded(_))), "a caller cannot expand the default decoded profile");
}

#[test]
fn mesh569_stl_rejects_truncation_trailing_bytes_huge_counts_and_empty_geometry() {
    let bytes = fixture();
    for length in 0..bytes.len() {
        assert!(decode_mesh_as(&bytes[..length], MeshFormat::Stl, &MeshLimits::default()).is_err(), "{length}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode_mesh_as(&trailing, MeshFormat::Stl, &MeshLimits::default()).is_err());
    for n in [0, 2, u32::MAX] {
        let mut hostile = bytes.clone();
        hostile[80..84].copy_from_slice(&n.to_le_bytes());
        assert!(decode_mesh_as(&hostile, MeshFormat::Stl, &MeshLimits::default()).is_err());
    }
    assert!(encode_stl(&Mesh::default(), 134).is_err());
}

#[test]
fn mesh569_stl_refuses_colour_attributes_nonfinite_values_and_degenerate_facets() {
    for offset in [84, 88, 92, 96, 100, 104, 108, 112, 116, 120, 124, 128] {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bytes = fixture();
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(decode_mesh(&bytes, &MeshLimits::default()).is_err());
        }
    }
    for attribute in [1u16, 0x8000, u16::MAX] {
        let mut bytes = fixture();
        bytes[132..134].copy_from_slice(&attribute.to_le_bytes());
        assert!(decode_mesh(&bytes, &MeshLimits::default()).is_err());
    }
    let mut mesh = triangle();
    mesh.indices = vec![0, 0, 0];
    assert!(encode_stl(&mesh, 134).is_err());
    mesh.indices = vec![0, 1, 9];
    assert!(encode_stl(&mesh, 134).is_err());
    let mut bytes = fixture();
    bytes[96..132].fill(0);
    assert!(decode_mesh(&bytes, &MeshLimits::default()).is_err());
}

#[test]
fn mesh569_stl_ascii_is_named_and_refused_instead_of_imported_as_text() {
    let ascii = b"solid triangle\n facet normal 0 0 1\n outer loop\n vertex 0 0 0\n vertex 1 0 0\n vertex 0 1 0\n endloop\n endfacet\nendsolid triangle\n";
    assert_eq!(crate::sniff(ascii).unwrap().mime, "model/stl");
    let error = decode_mesh(ascii, &MeshLimits::default()).unwrap_err().to_string();
    assert!(error.contains("ASCII STL"), "{error}");
    for prose in [b"solid foundations make a building\n".as_slice(), b"solid\nfacet normal is an expression\n", b"solid\nouter loop\nvertex positions\n"] {
        assert_ne!(crate::sniff(prose).map(|s| s.mime), Some("model/stl"));
    }
}
