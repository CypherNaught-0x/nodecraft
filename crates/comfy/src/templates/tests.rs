use super::*;

fn catalog() -> crate::NodeCatalog {
    let mut catalog = crate::NodeCatalog::parse(include_bytes!("fixtures/catalog-facts.json")).unwrap().0;
    catalog.extend(crate::NodeCatalog::parse(include_bytes!("fixtures/mock-core-nodes.json")).unwrap().0);
    catalog
}

fn fixtures() -> [(&'static str, &'static str); 7] {
    [
        ("video_minimax_h3_i2v", include_str!("fixtures/video_minimax_h3_i2v.json")),
        ("image_z_image_turbo", include_str!("fixtures/image_z_image_turbo.json")),
        ("video_wan2_2_14B_i2v", include_str!("fixtures/video_wan2_2_14B_i2v.json")),
        ("video_minimax_h3_r2v", include_str!("fixtures/video_minimax_h3_r2v.json")),
        ("api_nano_banana_pro", include_str!("fixtures/api_nano_banana_pro.json")),
        ("image_sdxl_simple", include_str!("fixtures/image_sdxl_simple.json")),
        ("audio_stable_audio_example", include_str!("fixtures/audio_stable_audio_example.json")),
    ]
}

#[test]
fn templates948_pinned_structural_fixtures_match_and_graph_edits_stop_curation() {
    assert_eq!(manifest().unwrap().profiles.len(), fixtures().len());
    for (id, fixture) in fixtures() {
        let mut root: Value = serde_json::from_str(fixture).unwrap();
        assert_eq!(matching(&root).unwrap(), Some(id), "{id}");
        // Display and widget values are the person's work, not part of template identity.
        root["id"] = Value::from("user-workflow-uuid");
        root["nodes"][0]["title"] = Value::from("User's own title");
        root["nodes"][0]["pos"] = serde_json::json!([123, 456]);
        if let Some(widgets) = root["nodes"][0]["widgets_values"].as_array_mut()
            && let Some(value) = widgets.first_mut()
        {
            *value = Value::from("User's own value");
        }
        assert_eq!(matching(&root).unwrap(), Some(id));
        root["nodes"][0]["mode"] = Value::from(2);
        assert_eq!(matching(&root).unwrap(), None, "execution mode matters: {id}");
        root["nodes"][0]["mode"] = Value::from(0);
        root["nodes"][0]["type"] = Value::from("ChangedNodeClass");
        assert_eq!(matching(&root).unwrap(), None, "node class matters: {id}");
    }
}

#[test]
fn templates948_deep_programmatic_shapes_are_bounded_before_copying() {
    let mut value = Value::Null;
    for _ in 0..1000 {
        value = Value::Array(vec![value]);
    }
    assert!(shape(&value).is_err());
    crate::json_bounds::dispose(value);
}

#[test]
fn templates948_pinned_image_audio_and_partner_graphs_convert_and_select_real_fields() {
    let catalog = catalog();
    for (id, fixture) in fixtures()
        .into_iter()
        .filter(|(id, _)| matches!(*id, "image_z_image_turbo" | "image_sdxl_simple" | "api_nano_banana_pro" | "audio_stable_audio_example"))
    {
        let converted = crate::ApiWorkflow::convert(fixture.as_bytes(), Some(&catalog)).unwrap();
        assert_eq!(converted.template, Some(id));
        let original = converted.workflow.clone();
        let mut exposed = crate::discover(&converted.workflow, &catalog).unwrap();
        crate::annotate_promoted(&mut exposed, &converted.promoted);
        let partners = exposed.partners.clone();
        assert!(curate_template(converted.template, &converted.promoted, &mut exposed).unwrap(), "{id}");
        let expected = manifest().unwrap().profiles.iter().find(|p| p.id == id).unwrap();
        assert_eq!(
            exposed.inputs.iter().filter(|i| !matches!(i.kind, ExposedKind::Media(_))).map(|i| i.label.as_str()).collect::<Vec<_>>(),
            expected.fields.iter().filter(|f| !matches!(f.kind, Kind::Media)).map(|f| f.label.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(exposed.partners, partners);
        assert_eq!(converted.workflow, original, "curation must not rewrite generation inputs");
        assert!(exposed.inputs.iter().all(|i| !matches!(converted.workflow.nodes[&i.node].inputs.get(&i.input), Some(crate::InputValue::Link { .. }))));
        let before = exposed.clone();
        let mut missing = exposed.clone();
        missing.inputs.clear();
        let unchanged = missing.clone();
        assert!(!curate_template(Some(id), &converted.promoted, &mut missing).unwrap());
        assert_eq!(missing, unchanged, "no partial application on server mismatch");
        assert!(!curate_template(None, &converted.promoted, &mut exposed).unwrap());
        assert_eq!(exposed, before);
        if id == "api_nano_banana_pro" {
            assert_eq!(exposed.partners.len(), 1, "paid credit evidence survives selection");
            assert_eq!(exposed.inputs.iter().filter(|i| matches!(i.kind, ExposedKind::Media(_))).count(), 2, "both reference images remain uploads");
        }
    }
}
