use super::*;
use proptest::prelude::*;

const INDEPENDENT: &[u8] = include_bytes!("fixtures/original-indexed.usdz");
const LAYER: &str = include_str!("fixtures/original-indexed.usda");
fn indexed() -> Mesh {
    Mesh {
        positions: vec![[-1., 0., 0.], [1., 0., 0.], [1., 1., 0.], [-1., 1., 0.]],
        normals: vec![[0., 0., 1.]; 4],
        colors: vec![[1., 0., 0., 0.], [0., 1., 0., 0.25], [0., 0., 1., 0.5], [1., 1., 0., 1.]],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}
#[test]
fn usdz1019_independent_apple_package_preserves_sharing_winding_and_linear_rgba() {
    assert_eq!(crate::sniff(INDEPENDENT).unwrap().mime, "model/vnd.usdz+zip");
    let mesh = decode_mesh(INDEPENDENT, &Default::default()).unwrap();
    assert_eq!(mesh, indexed());
    assert_eq!(mesh.bounds(), Some(Bounds { min: [-1., 0., 0.], max: [1., 1., 0.] }));
    assert_eq!(MeshFormat::from_mime("model/vnd.usdz+zip"), Some(MeshFormat::Usdz));
    assert_eq!(MeshFormat::Usdz.extension(), "usdz");
    let bytes = encode_usdz(&mesh, 1 << 20).unwrap();
    assert_eq!(decode_mesh(&bytes, &Default::default()).unwrap(), mesh);
    assert_eq!(encode_usdz(&mesh, 1 << 20).unwrap(), bytes, "deterministic archive metadata");
    let glb = encode_glb(&mesh).unwrap();
    assert_eq!(decode_mesh(&glb, &Default::default()).unwrap().positions, mesh.positions);
}
#[test]
fn usdz1019_full_size_is_preflighted_including_zip_padding_and_footer() {
    let mesh = indexed();
    let bytes = encode_usdz(&mesh, 1 << 20).unwrap();
    let n = bytes.len();
    assert_eq!(n, usdz::layer(&bytes).unwrap().len() + 142);
    usdz::reset_allocations();
    assert!(encode_usdz(&mesh, n - 1).is_err());
    assert_eq!(usdz::allocations(), [0, 0]);
    assert_eq!(encode_usdz(&mesh, n).unwrap(), bytes);
    let mut limits = MeshLimits { max_input_bytes: n - 1, ..Default::default() };
    usdz::reset_allocations();
    assert!(decode_mesh(&bytes, &limits).is_err());
    assert_eq!(usdz::allocations(), [0, 0]);
    limits.max_input_bytes = n;
    assert_eq!(decode_mesh(&bytes, &limits).unwrap(), mesh);
    for limits in [MeshLimits { max_vertices: 3, ..Default::default() }, MeshLimits { max_triangles: 1, ..Default::default() }] {
        usdz::reset_allocations();
        assert!(decode_mesh(INDEPENDENT, &limits).is_err());
        assert_eq!(usdz::allocations(), [0, 0]);
    }
}
fn refuse_layer(layer: &str) {
    let bytes = usdz::package_layer(layer.as_bytes()).unwrap();
    usdz::reset_allocations();
    assert!(decode_mesh_as(&bytes, MeshFormat::Usdz, &Default::default()).is_err(), "{layer}");
    assert_eq!(usdz::allocations(), [0, 0], "grammar/count/attribute refusal precedes geometry allocation");
}
#[test]
fn usdz1019_rejects_unsupported_schema_and_full_body_before_geometry_allocation() {
    for (from, to) in [
        ("upAxis = \"Y\"", "upAxis = \"Z\""),
        ("metersPerUnit = 1", "metersPerUnit = 0.01"),
        ("metersPerUnit = 1", "metersPerUnit = 1.00000001"),
        ("def Mesh \"Square\"", "def Xform \"Square\""),
        ("orientation = \"rightHanded\"", "orientation = \"leftHanded\""),
        ("subdivisionScheme = \"none\"", "subdivisionScheme = \"catmullClark\""),
        ("[3, 3]", "[3, 4]"),
        ("[0, 1, 2, 0, 2, 3]", "[0, 1, 2, 0, 2, 4]"),
        ("(0, 0, 1)", "(0, 0, 0)"),
        ("(-1, 0, 0)", "(NaN, 0, 0)"),
        ("(-1, 0, 0)", "(1e999, 0, 0)"),
        ("(-1, 0, 0)", "(1e-999, 0, 0)"),
        ("[0, 0.25, 0.5, 1]", "[0, 0.25, 0.5, 2]"),
        ("interpolation = \"vertex\"", "interpolation = \"faceVarying\""),
        ("    uniform token subdivisionScheme", "    asset texture = @file:///private@\n    uniform token subdivisionScheme"),
        ("    int[] faceVertexCounts", "    float timeSamples = 1\n    int[] faceVertexCounts"),
        ("    point3f[] points", "    int[] faceVertexCounts = [3, 3]\n    point3f[] points"),
    ] {
        assert!(LAYER.contains(from));
        refuse_layer(&LAYER.replace(from, to));
    }
    refuse_layer(&format!("{LAYER}\ndef Mesh \"Extra\" {{}}"));
    refuse_layer(&format!("{LAYER}#{}\n", "x".repeat(1025)));
    refuse_layer(&LAYER.replace("Square", &"x".repeat(65)));
    for n in [0, 1, LAYER.len() - 3] {
        refuse_layer(LAYER.get(..n).unwrap());
    }
}
#[test]
fn usdz1019_container_authorities_crc_alignment_and_extents_are_all_checked() {
    let bytes = encode_usdz(&indexed(), 1 << 20).unwrap();
    let cd = bytes.len() - 78;
    for (at, value) in [
        (4, 45),
        (6, 1),
        (8, 8),
        (26, 255),
        (28, 0),
        (40, 0),
        (64, 0),
        (cd + 8, 1),
        (cd + 10, 8),
        (cd + 16, 0),
        (cd + 38, 16),
        (cd + 42, 1),
        (bytes.len() - 12, 2),
        (bytes.len() - 6, 1),
        (bytes.len() - 1, 1),
    ] {
        let mut bad = bytes.clone();
        bad[at] = value;
        usdz::reset_allocations();
        assert!(decode_mesh_as(&bad, MeshFormat::Usdz, &Default::default()).is_err(), "offset{at}");
        assert_eq!(usdz::allocations(), [0, 0]);
    }
    for n in 0..bytes.len() {
        assert!(decode_mesh_as(&bytes[..n], MeshFormat::Usdz, &Default::default()).is_err(), "truncation{n}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode_mesh(&trailing, &Default::default()).is_err());
    let mut path = bytes.clone();
    path[30] = b'/';
    assert!(decode_mesh_as(&path, MeshFormat::Usdz, &Default::default()).is_err());
    let mut zip = vec![0; 100];
    zip[..4].copy_from_slice(b"PK\x03\x04");
    assert!(crate::sniff(&zip).is_none());
    let mut stl = encode_stl(&indexed(), 1 << 20).unwrap();
    stl[..80].copy_from_slice(&bytes[..80]);
    assert_eq!(crate::sniff(&stl).unwrap().mime, "model/stl", "arbitrary binary STL headers still win");
}
#[test]
fn usdz1019_indexed_facets_are_validated_before_adoption_and_limits_cannot_widen() {
    assert!(encode_usdz(&Mesh::default(), 1 << 20).is_err());
    let mut no_faces = indexed();
    no_faces.indices.clear();
    assert!(encode_usdz(&no_faces, 1 << 20).is_err());
    let layer = LAYER.replace("[0, 1, 2, 0, 2, 3]", "[0, 1, 1, 0, 2, 3]");
    usdz::reset_allocations();
    let bad = usdz::package_layer(layer.as_bytes()).unwrap();
    assert!(decode_mesh(&bad, &Default::default()).is_err());
    assert_eq!(usdz::allocations()[0], 1, "area checks use only admitted indexed scratch");
    let mut m = indexed();
    m.indices[2] = 1;
    usdz::reset_allocations();
    assert!(encode_usdz(&m, 1 << 20).is_err());
    assert_eq!(usdz::allocations(), [0, 0]);
    for bad in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        let mut m = indexed();
        m.colors[0][0] = bad;
        assert!(encode_usdz(&m, 1 << 20).is_err());
    }
    let mut m = indexed();
    m.normals[0] = [0.; 3];
    assert!(encode_usdz(&m, 1 << 20).is_err());
    let mut m = indexed();
    m.positions.push([0.; 3]);
    assert!(encode_usdz(&m, 1 << 20).is_err());
}
#[test]
fn usdz1019_f64_bounds_avoid_intermediate_overflow_and_reject_unrepresentable_spheres() {
    let b = Bounds { min: [1e30, 1e30, 0.], max: [2e30, 2e30, 0.] };
    assert!(b.center().iter().all(|v| v.is_finite()));
    assert!(b.radius().is_finite());
    let (center, radius) = b.checked_sphere().unwrap();
    assert_eq!(center, b.center());
    assert!((f64::from(radius) / (2f64.sqrt() * 0.5e30) - 1.0).abs() < 1e-6);
    let same = Bounds { min: [f32::MAX; 3], max: [f32::MAX; 3] };
    assert_eq!(same.center(), [f32::MAX; 3]);
    assert_eq!(same.radius(), 0.);
    let span = Bounds { min: [-f32::MAX; 3], max: [f32::MAX; 3] };
    assert!(span.checked_sphere().is_none());
    assert!(span.radius().is_infinite());
    assert!(Bounds { min: [0.; 3], max: [f32::from_bits(1), 0., 0.] }.checked_sphere().is_none(), "a nonzero sphere must not silently round to zero");
    let mut m = indexed();
    m.positions = vec![[1e30, 1e30, 0.], [2e30, 1e30, 0.], [2e30, 2e30, 0.], [1e30, 2e30, 0.]];
    let bytes = encode_usdz(&m, 1 << 20).unwrap();
    assert_eq!(decode_mesh(&bytes, &Default::default()).unwrap(), m);
    let mut m = indexed();
    m.positions[0] = [-f32::MAX; 3];
    m.positions[1] = [f32::MAX; 3];
    assert!(encode_usdz(&m, 1 << 20).is_err());
}
proptest! {
    #[test]
    fn usdz1019_generated_triangle_roundtrips(x in -100f32..100.,y in -100f32..100.,sx in 1f32..10.,sy in 1f32..10.,alpha in 0f32..1.){
        let m=Mesh{positions:vec![[x,y,0.],[x+sx,y,0.],[x,y+sy,0.]],normals:vec![[0.,0.,1.];3],colors:vec![[0.25,0.5,0.75,alpha];3],indices:vec![0,1,2]};
        let b=encode_usdz(&m,1<<20).unwrap();prop_assert_eq!(decode_mesh(&b,&Default::default()).unwrap(),m);
    }
    #[test]
    fn usdz1019_bounded_hostile_bytes_return_without_panic(bytes in prop::collection::vec(any::<u8>(),0..4096)){
        let _=decode_mesh_as(&bytes,MeshFormat::Usdz,&MeshLimits{max_input_bytes:4096,max_vertices:32,max_triangles:32,..Default::default()});
    }
}
#[test]
#[ignore = "write original USDZ samples for installed independent USD tools"]
fn usdz1019_independent_output_samples() {
    let dir = std::path::PathBuf::from(std::env::var_os("NODECRAFT_USDZ1019_PROOF_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("indexed.usdz"), encode_usdz(&indexed(), 1 << 20).unwrap()).unwrap();
    let m = decode_mesh(INDEPENDENT, &Default::default()).unwrap();
    std::fs::write(dir.join("reencoded-independent.usdz"), encode_usdz(&m, 1 << 20).unwrap()).unwrap();
}
