use super::*;
const QUAD: &[u8] = b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
#[test]
fn prepared_native_and_browser_geometry_is_identical() {
    let native = prepare(QUAD, Profile::Native).unwrap();
    let browser = prepare(QUAD, Profile::Browser).unwrap();
    assert_eq!(native.layout(), browser.layout());
    assert_eq!(native.buffers(), browser.buffers());
    assert_eq!(native.bounds(), browser.bounds());
    assert_eq!(native.center(), [0.5, 0.5, 0.]);
    assert_eq!(native.layout().triangles, 2);
    assert_eq!(native.layout().edge_words, 10);
}
#[test]
fn hostile_counts_extents_and_bounds_are_refused_before_reserving() {
    let bounds = Bounds { min: [0.; 3], max: [1.; 3] };
    for layout in [
        Layout { vertices: u32::MAX, triangles: 1, edge_words: 0 },
        Layout { vertices: 1, triangles: u32::MAX, edge_words: 0 },
        Layout { vertices: 1, triangles: 1, edge_words: u32::MAX },
        Layout { vertices: 0, triangles: 0, edge_words: 0 },
    ] {
        assert!(Adoption::new(layout, bounds, [usize::MAX; 3]).is_err());
    }
    let valid = Layout { vertices: 3, triangles: 1, edge_words: 6 };
    assert!(Adoption::new(valid, bounds, [120, 12, 23]).is_err());
    assert!(Adoption::new(valid, Bounds { min: [f32::NAN; 3], max: [1.; 3] }, [120, 12, 24]).is_err());
}
#[test]
fn incoming_geometry_is_incrementally_validated_before_becoming_drawable() {
    let model = prepare(QUAD, Profile::Browser).unwrap();
    let mut adoption = Adoption::new(model.layout(), model.bounds(), model.buffers.each_ref().map(Vec::len)).unwrap();
    assert!(adoption.push(1, &[0; 4]).is_err());
    for (i, bytes) in model.buffers().into_iter().enumerate() {
        let record = if i == 0 { 40 } else { 4 };
        for chunk in bytes.chunks(record) {
            adoption.push(i, chunk).unwrap();
        }
    }
    let result = adoption.finish().unwrap();
    assert_eq!(result.buffers(), model.buffers());
    let mut bad = Adoption::new(model.layout(), model.bounds(), model.buffers.each_ref().map(Vec::len)).unwrap();
    let mut record = model.buffers[0].get(..40).unwrap().to_vec();
    record.get_mut(..4).unwrap().copy_from_slice(&f32::INFINITY.to_le_bytes());
    assert_eq!(bad.push(0, &record), Err(Error::Invalid));
    assert!(bad.finish().is_err());
}
#[test]
fn malformed_bytes_empty_models_and_bounds_overflow_are_errors() {
    for input in [b"".as_slice(), b"glTF\xff\xff\xff\xff", b"v 0 0 0\n", b"{\"buffers\":[{\"uri\":\"https://example.com\"}]}"] {
        assert!(prepare(input, Profile::Browser).is_err());
    }
    assert!(view(Bounds { min: [-f32::MAX; 3], max: [f32::MAX; 3] }).is_err());
}
#[test]
fn browser_profile_and_full_payload_ceiling_are_explicit() {
    let p = Profile::Browser;
    let layout = Layout { vertices: 600_000, triangles: 300_000, edge_words: 1_800_000 };
    assert_eq!(layout.sizes(p).unwrap(), [24_000_000, 3_600_000, 7_200_000]);
    assert_eq!(layout.sizes(p).unwrap().into_iter().sum::<usize>(), 34_800_000);
    assert_eq!(p.limits().max_input_bytes, 16 * 1024 * 1024);
}
#[test]
fn oversized_chunks_partial_records_bad_indices_and_false_bounds_never_adopt() {
    let model = prepare(QUAD, Profile::Browser).unwrap();
    let make = || Adoption::new(model.layout(), model.bounds(), model.buffers.each_ref().map(Vec::len)).unwrap();
    assert!(make().push(0, &vec![0; ADOPT_BYTES + 40]).is_err());
    assert!(make().push(0, &[0; 39]).is_err());
    let mut out = make();
    out.push(0, model.buffers()[0]).unwrap();
    assert_eq!(out.push(1, &u32::MAX.to_le_bytes()), Err(Error::Invalid));
    let mut changed = model.bounds();
    changed.max[2] = 1.;
    let mut out = Adoption::new(model.layout(), changed, model.buffers.each_ref().map(Vec::len)).unwrap();
    for (i, bytes) in model.buffers().into_iter().enumerate() {
        if !bytes.is_empty() {
            out.push(i, bytes).unwrap();
        }
    }
    assert!(out.finish().is_err());
}

#[test]
fn original_nine_megabyte_model_matches_native_reference_and_preserves_bytes() {
    let bytes = crate::fixture::large();
    assert_eq!(bytes.len(), 9_000_084);
    assert!(bytes.len() > 8 * 1024 * 1024);
    let original = blake3::hash(&bytes);
    let a = prepare(&bytes, Profile::Native).unwrap();
    let b = prepare(&bytes, Profile::Browser).unwrap();
    assert_eq!(a.layout(), b.layout());
    assert_eq!(a.buffers(), b.buffers());
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(a.layout(), Layout { vertices: 540_000, triangles: 180_000, edge_words: 1_080_000 });
    assert_eq!(a.bounds(), Bounds { min: [0.; 3], max: [1.171875, 1.171875, 0.5859375] });
    assert_eq!(a.bytes(), 28_080_000);
    assert_eq!(original, blake3::hash(&bytes));
    println!(
        "original={} prepared={} bytes={} triangles={} vertices={} edge_words={}",
        original,
        blake3::Hash::from_bytes(a.fingerprint()),
        a.bytes(),
        a.layout().triangles,
        a.layout().vertices,
        a.layout().edge_words
    );
}

#[test]
fn renderer_admission_runs_before_packing_without_changing_interchange() {
    let original = blake3::hash(QUAD);
    assert_eq!(
        prepare_checked(QUAD, Profile::Browser, |center, radius| {
            assert_eq!(center, [0.5, 0.5, 0.]);
            assert!(radius.is_finite());
            false
        })
        .unwrap_err(),
        Error::Coordinates
    );
    assert_eq!(blake3::hash(QUAD), original);
    assert!(prepare(QUAD, Profile::Browser).is_ok());
}
