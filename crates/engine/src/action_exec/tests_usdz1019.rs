use super::*;

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
    assert!(610_000usize * (11 + 17) > nodecraft_format::MAX_ASSET_BYTES);
    let mut assets = AssetStore::default();
    let hash = assets.insert(&bytes).unwrap();
    let actions = LocalActions::new(assets);
    let input = Value::Media(AssetRef { hash, mime: "model/gltf-binary".parse().unwrap(), meta: Default::default() });
    let params = ParamValues::from([("format".into(), Value::Text("usdz".into()))]);
    let error = actions.execute(nodecraft_nodes::mesh::CONVERT, &params, &[input]).unwrap_err();
    assert!(error.contains("output exceeds its byte limit"), "{error}");
    assert_eq!(actions.assets.get(&hash).unwrap(), bytes);
}
