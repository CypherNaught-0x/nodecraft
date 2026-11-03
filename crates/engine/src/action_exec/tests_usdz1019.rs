use super::*;

#[test]
fn usdz1019_programmatic_photo_views_and_turntable_refuse_invalid_projection_without_tools() {
    let mesh = nodecraft_media::Mesh {
        positions: vec![[1e30, 1e30, 0.], [2e30, 1e30, 0.], [2e30, 2e30, 0.], [1e30, 2e30, 0.]],
        normals: vec![[0., 0., 1.]; 4],
        colors: vec![[1., 0.25, 0.5, 1.]; 4],
        indices: vec![0, 1, 2, 0, 2, 3],
    };
    let bytes = nodecraft_media::encode_usdz(&mesh, 1 << 20).unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&bytes).unwrap();
    let actions =
        LocalActions::new(assets).with_video_tools(Some(("nodecraft-usdz-must-not-execute-ffmpeg".into(), "nodecraft-usdz-must-not-execute-ffprobe".into())));
    let input = Value::Media(AssetRef { hash, mime: "model/vnd.usdz+zip".parse().unwrap(), meta: Default::default() });
    let kinds = Registry::builtins().unwrap();
    for action in [nodecraft_nodes::mesh::PHOTO, nodecraft_nodes::mesh::VIEWS, nodecraft_nodes::mesh::TURNTABLE] {
        let mut p = defaults(&kinds, action);
        for name in ["width", "height"] {
            if p.contains_key(name) {
                p.insert(name.into(), Value::Unsigned(nodecraft_types::Unsigned(16)));
            }
        }
        if action == nodecraft_nodes::mesh::VIEWS {
            p.insert("size".into(), Value::Unsigned(nodecraft_types::Unsigned(64)));
            p.insert("count".into(), Value::Text("4".into()));
        }
        if action == nodecraft_nodes::mesh::TURNTABLE {
            p.insert("fps".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
            p.insert("duration".into(), Value::Number(nodecraft_types::Number::new(1.).unwrap()));
        }
        let error = actions.execute(action, &p, std::slice::from_ref(&input)).unwrap_err();
        assert!(error.contains("coordinate range"), "{action}: {error}");
        assert_eq!(actions.assets.get(&hash).unwrap(), bytes);
    }
    let converted = actions.execute(nodecraft_nodes::mesh::CONVERT, &ParamValues::from([("format".into(), Value::Text("glb".into()))]), &[input]).unwrap();
    let Value::Media(converted) = converted else { panic!("ordinary conversion remains available") };
    let staged = actions.take_staged();
    assert_eq!(nodecraft_media::decode_mesh(staged.get(&converted.hash).unwrap(), &Default::default()).unwrap(), mesh);
}

#[test]
fn usdz1019_convert_action_keeps_its_actual_16mib_output_cap_and_source() {
    let mesh = nodecraft_media::Mesh {
        positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
        normals: vec![[0., 0., 1.]; 3],
        colors: vec![[1., 0.5, 0.25, 1.]; 3],
        indices: [0u32, 1, 2].repeat(610_000),
    };
    let bytes = nodecraft_media::encode_glb(&mesh).unwrap();
    assert!(bytes.len() < nodecraft_format::MAX_ASSET_BYTES, "indexed source fits the existing input allowance");
    // Even before positions/attributes and ZIP overhead, these610k count/index rows need
    //610000*(11+17)>16MiB. The actual action must pass its cap to the counted writer.
    assert!(mesh.triangle_count() * (11 + 17) > nodecraft_format::MAX_ASSET_BYTES);
    let mut assets = AssetStore::default();
    let hash = assets.insert(&bytes).unwrap();
    let actions = LocalActions::new(assets);
    let input = Value::Media(AssetRef { hash, mime: "model/gltf-binary".parse().unwrap(), meta: Default::default() });
    let params = ParamValues::from([("format".into(), Value::Text("usdz".into()))]);
    let error = actions.execute(nodecraft_nodes::mesh::CONVERT, &params, &[input]).unwrap_err();
    assert!(error.contains("output exceeds its byte limit"), "{error}");
    assert_eq!(actions.assets.get(&hash).unwrap(), bytes);
}
