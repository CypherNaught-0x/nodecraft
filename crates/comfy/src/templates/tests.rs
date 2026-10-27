use super::*;

fn catalog() -> crate::NodeCatalog {
    let mut catalog = crate::NodeCatalog::parse(include_bytes!("fixtures/catalog-facts.json")).unwrap().0;
    catalog.extend(crate::NodeCatalog::parse(include_bytes!("fixtures/mock-core-nodes.json")).unwrap().0);
    catalog
}

fn projected_catalog() -> crate::NodeCatalog {
    let mut catalog = catalog();
    catalog.extend(crate::NodeCatalog::parse(include_bytes!("fixtures/documented-nodes.json")).unwrap().0);
    catalog.extend(crate::NodeCatalog::parse(include_bytes!("fixtures/pinned-selector-facts.json")).unwrap().0);
    catalog
}

fn fixtures() -> [(&'static str, &'static str); 10] {
    [
        ("video_ltx2_3_i2v", include_str!("fixtures/video_ltx2_3_i2v.json")),
        ("image_qwen_image_2_1_image_edit", include_str!("fixtures/image_qwen_image_2_1_image_edit.json")),
        ("image_krea2_turbo_t2i", include_str!("fixtures/image_krea2_turbo_t2i.json")),
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
        let check = converted.workflow.check(&catalog);
        assert!(check.is_ok(), "{id}: {:?}", check.problems);
        crate::PromptRequest::new(converted.workflow.clone(), "offline-template-contract").to_json().unwrap();
        let original = converted.workflow.clone();
        let mut exposed = crate::discover(&converted.workflow, &catalog).unwrap();
        crate::annotate_promoted(&mut exposed, &converted.promoted);
        let partners = exposed.partners.clone();
        assert!(curate_template(converted.template, &converted.promoted, &mut exposed).unwrap(), "{id}");
        let expected = manifest().unwrap().profiles.iter().find(|p| p.id == id).unwrap();
        assert_eq!(
            exposed.inputs.iter().filter(|i| !matches!(i.kind, ExposedKind::Media(_))).map(|i| i.label.as_str()).collect::<Vec<_>>(),
            expected.fields.iter().filter(|f| !matches!(f.kind, Kind::Media)).map(|f| f.label.as_deref().unwrap_or_default()).collect::<Vec<_>>()
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

#[test]
fn templates948_ordinal_selector_keeps_server_labels_and_rejects_shifted_widget_contracts() {
    for names in [["ratio", "area", "align"], ["aspect_ratio", "megapixels", "multiple_of"]] {
        let raw = serde_json::json!({
          "Controls":{"input":{"required":{"prompt":["STRING",{}],"seconds":["FLOAT",{}],"seed":["INT",{}]}}},
          "Selector":{"input":{"required":{names[0]:["COMBO",{"options":["1:1"]}],names[1]:["FLOAT",{}],names[2]:["INT",{}]}},"input_order":{"required":names}},
          "SaveVideo":{"input":{"required":{"filename_prefix":["STRING",{}]}} ,"output_node":true}
        });
        let c = crate::NodeCatalog::parse(&serde_json::to_vec(&raw).unwrap()).unwrap().0;
        let w = crate::ApiWorkflow::parse(
            &serde_json::to_vec(&serde_json::json!({
              "10":{"class_type":"Controls","inputs":{"prompt":"Own prompt","seconds":5.0,"seed":7}},
              "115":{"class_type":"Selector","inputs":{names[0]:"1:1",names[1]:0.4,names[2]:32}},
              "92":{"class_type":"SaveVideo","inputs":{"filename_prefix":"NodeCraft"}}
            }))
            .unwrap(),
            None,
        )
        .unwrap();
        let fields = crate::discover(&w, &c).unwrap();
        let promoted = [("prompt", "prompt"), ("value_1", "seconds"), ("noise_seed", "seed")].map(|(name, input)| PromotedInput {
            instance: "105".into(),
            name: name.into(),
            label: name.into(),
            kind: String::new(),
            targets: vec![("10".into(), input.into())],
        });
        let mut selected = fields.clone();
        assert!(curate_template(Some("video_minimax_h3_i2v"), &promoted, &mut selected).unwrap());
        let size = selected.inputs.iter().filter(|i| i.node == "115").collect::<Vec<_>>();
        assert_eq!(size.len(), 2);
        assert!(size[0].label.ends_with(names[0]) && size[1].label.ends_with(names[1]));
        assert_eq!(size[1].current, Some(serde_json::json!(0.4)));
        let mut shifted = fields.clone();
        shifted.inputs.retain(|i| !(i.node == "115" && i.input == names[2]));
        let before = shifted.clone();
        assert!(!curate_template(Some("video_minimax_h3_i2v"), &promoted, &mut shifted).unwrap());
        assert_eq!(shifted, before);
        let mut wrong_kind = fields;
        wrong_kind.inputs.iter_mut().find(|i| i.node == "115" && i.input == names[1]).unwrap().kind = ExposedKind::Bool;
        let before = wrong_kind.clone();
        assert!(!curate_template(Some("video_minimax_h3_i2v"), &promoted, &mut wrong_kind).unwrap());
        assert_eq!(wrong_kind, before);
    }
}

#[test]
fn templates948_documented_candidates_convert_check_and_serialize_offline() {
    let catalog = projected_catalog();
    let mut problems = Vec::new();
    for (id, fixture) in fixtures() {
        let converted = match crate::ApiWorkflow::convert(fixture.as_bytes(), Some(&catalog)) {
            Ok(converted) => converted,
            Err(error) => {
                problems.push(format!("{id}: conversion: {error}"));
                continue;
            }
        };
        let mut fields = crate::discover(&converted.workflow, &catalog).unwrap();
        crate::annotate_promoted(&mut fields, &converted.promoted);
        if !curate_template(converted.template, &converted.promoted, &mut fields).unwrap() {
            problems.push(format!("{id}: field selection differs from the documented projection"));
            continue;
        }
        let roots = fields.outputs.iter().map(|output| output.node.as_str()).collect::<Vec<_>>();
        let mut request_workflow = converted.workflow.clone();
        request_workflow.retain_upstream(&roots).unwrap();
        let check = request_workflow.check(&catalog);
        if !check.is_ok() {
            problems.push(format!("{id}: check: {:?}", check.problems));
            continue;
        }
        let request = crate::PromptRequest::new(request_workflow, "original-offline-fixture").to_json().unwrap();
        assert!(!request["prompt"].as_object().unwrap().is_empty());
        if id == "video_minimax_h3_i2v" {
            assert!(request["prompt"].get("119").is_none(), "unconnected sizing helper is not scheduled");
        }
        if id == "image_qwen_image_2_1_image_edit" {
            assert!(request["prompt"].get("472").is_none(), "unselected custom comparison is not scheduled");
            assert_eq!(request["prompt"]["459:469"]["inputs"]["device"], "auto");
            assert_eq!(request["prompt"]["459:469"]["inputs"]["dtype"], "default");
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn templates948_documentation_selector_spelling_is_not_claimed_to_match_the_pin() {
    let mut docs = catalog();
    docs.extend(crate::NodeCatalog::parse(include_bytes!("fixtures/documented-nodes.json")).unwrap().0);
    let fixture = include_bytes!("fixtures/video_ltx2_3_i2v.json");
    assert!(crate::ApiWorkflow::convert(fixture, Some(&docs)).is_err(), "current documentation uses different selector spellings than the pinned saved graph");
    let subset: Value = serde_json::from_slice(include_bytes!("fixtures/pinned-selector-facts.json")).unwrap();
    assert_eq!(
        subset["ResizeImageMaskNode"]["input"]["required"]["resize_type"][1]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["scale dimensions", "scale longer dimension"]
    );
}
