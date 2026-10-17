use nodecraft_types::{
    AssetRef, Blake3, Choice, Integer, MediaMeta, Modality, Number, ParamControl, ParamSchema, ParamSpec, ParamValues, Unsigned, Value, VisibleIf,
};

fn number(value: f64) -> Number {
    Number::new(value).unwrap()
}
fn spec(name: &str, control: ParamControl, default: Option<Value>) -> ParamSpec {
    ParamSpec { name: name.into(), label: name.into(), description: None, control, required: true, default, visible_if: None }
}
fn asset(mime: &str) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([1; 32]), mime: mime.parse().unwrap(), meta: MediaMeta::default() })
}

#[test]
fn every_control_validates_its_default_and_rejects_wrong_types_and_bounds() {
    let cases = vec![
        (ParamControl::Text { multiline: true, max_bytes: 4 }, Value::Text("éé".into()), Value::Text("ééé".into())),
        (ParamControl::Choice { options: vec![Choice { value: "one".into(), label: "One".into() }] }, Value::Text("one".into()), Value::Text("two".into())),
        (ParamControl::Toggle, Value::Bool(false), Value::Text("false".into())),
        (ParamControl::Slider { min: Some(number(-1.0)), max: Some(number(1.0)), step: number(0.1) }, Value::Number(number(0.25)), Value::Number(number(2.0))),
        (
            ParamControl::Integer { min: Some(Integer(i64::MIN)), max: Some(Integer(100)), step: Unsigned(1) },
            Value::Integer(Integer(i64::MIN)),
            Value::Integer(Integer(101)),
        ),
        (ParamControl::Color, Value::Text("#aAbBcc80".into()), Value::Text("#gg0000".into())),
        (ParamControl::Model, Value::Text("provider/model-v1".into()), Value::Text("contains space".into())),
        (ParamControl::Seed { max: None }, Value::Unsigned(Unsigned(u64::MAX)), Value::Integer(Integer(-1))),
        (ParamControl::Asset { modality: Modality::Image }, asset("image/png;foo=bar"), asset("audio/wav")),
    ];
    for (control, good, bad) in cases {
        let mut schema = ParamSchema { params: vec![spec("value", control, Some(good.clone()))] };
        schema.validate().unwrap();
        let decoded: ParamSchema = serde_json::from_str(&serde_json::to_string(&schema).unwrap()).unwrap();
        assert_eq!(decoded, schema);
        assert_eq!(schema.resolve(&ParamValues::new(), true).unwrap().values.get("value"), Some(&good));
        assert!(schema.resolve(&ParamValues::from([("value".into(), bad.clone())]), true).is_err());
        schema.params.get_mut(0).unwrap().default = Some(bad);
        assert!(schema.validate().is_err());
    }
}

#[test]
fn visibility_uses_defaults_overrides_and_controller_visibility() {
    let enabled = spec("enabled", ParamControl::Toggle, Some(Value::Bool(false)));
    let mut secondary = spec("secondary", ParamControl::Toggle, Some(Value::Bool(true)));
    secondary.visible_if = Some(VisibleIf { parameter: "enabled".into(), equals: Value::Bool(true) });
    let mut text = spec("text", ParamControl::Text { multiline: true, max_bytes: 100 }, None);
    text.visible_if = Some(VisibleIf { parameter: "secondary".into(), equals: Value::Bool(true) });
    let schema = ParamSchema { params: vec![enabled, secondary, text] };
    let hidden = schema.resolve(&ParamValues::new(), true).unwrap();
    assert_eq!(hidden.visible.into_iter().collect::<Vec<_>>(), vec!["enabled"]);
    let supplied = ParamValues::from([("enabled".into(), Value::Bool(true))]);
    assert!(schema.resolve(&supplied, true).is_err()); // visible required text is missing
    let partial = schema.resolve(&supplied, false).unwrap();
    assert_eq!(partial.visible.len(), 3);
    let ready = ParamValues::from([("enabled".into(), Value::Bool(true)), ("text".into(), Value::Text("hello".into()))]);
    assert_eq!(schema.resolve(&ready, true).unwrap().values.get("text"), ready.get("text"));
    let off = ParamValues::from([("enabled".into(), Value::Bool(true)), ("secondary".into(), Value::Bool(false))]);
    assert!(!schema.resolve(&off, true).unwrap().visible.contains("text"));
}

#[test]
fn rejects_invalid_schema_definitions_and_visibility_dependencies() {
    let base = spec("switch", ParamControl::Toggle, Some(Value::Bool(true)));
    for control in [
        ParamControl::Text { multiline: false, max_bytes: 1024 * 1024 + 1 },
        ParamControl::Choice { options: vec![] },
        ParamControl::Choice { options: vec![Choice { value: "x".into(), label: "X".into() }; 2] },
        ParamControl::Slider { min: Some(number(1.0)), max: Some(number(0.0)), step: number(1.0) },
        ParamControl::Slider { min: Some(number(0.0)), max: Some(number(1.0)), step: number(0.0) },
        ParamControl::Integer { min: Some(Integer(2)), max: Some(Integer(1)), step: Unsigned(1) },
    ] {
        let schema = ParamSchema { params: vec![spec("bad", control, None)] };
        assert!(schema.validate().is_err());
    }
    assert!(ParamSchema { params: vec![base.clone(), base.clone()] }.validate().is_err());
    for condition in [
        VisibleIf { parameter: "missing".into(), equals: Value::Bool(true) },
        VisibleIf { parameter: "child".into(), equals: Value::Bool(true) },
        VisibleIf { parameter: "switch".into(), equals: Value::Text("true".into()) },
    ] {
        let mut child = spec("child", ParamControl::Toggle, Some(Value::Bool(false)));
        child.visible_if = Some(condition);
        assert!(ParamSchema { params: vec![base.clone(), child] }.validate().is_err());
    }
    let mut forward = base.clone();
    forward.visible_if = Some(VisibleIf { parameter: "later".into(), equals: Value::Bool(true) });
    assert!(ParamSchema { params: vec![forward, spec("later", ParamControl::Toggle, None)] }.validate().is_err());
    assert!(ParamSchema { params: vec![base; 257] }.validate().is_err());
}

#[test]
fn unconfigured_assets_stay_absent_and_mismatched_media_are_rejected() {
    for (modality, mime) in [
        (Modality::Image, "image/webp"),
        (Modality::Video, "video/mp4"),
        (Modality::Audio, "audio/ogg"),
        (Modality::Mesh, "model/gltf-binary"),
        (Modality::Document, "application/pdf"),
        (Modality::Text, "text/plain"),
    ] {
        let schema = ParamSchema { params: vec![spec("asset", ParamControl::Asset { modality }, None)] };
        assert!(schema.resolve(&ParamValues::new(), false).unwrap().values.is_empty());
        assert!(schema.resolve(&ParamValues::new(), true).is_err());
        schema.resolve(&ParamValues::from([("asset".into(), asset(mime))]), true).unwrap();
        assert!(schema.resolve(&ParamValues::from([("asset".into(), asset("application/octet-stream"))]), true).is_err());
    }
}

#[test]
fn hostile_values_unknown_parameters_and_aggregate_limits_fail_before_clone() {
    let mut deep = Value::Text("leaf".into());
    for _ in 0..128 {
        deep = Value::List(vec![deep]);
    }
    let mut schema = ParamSchema { params: vec![spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, None)] };
    assert!(schema.resolve(&ParamValues::from([("text".into(), deep.clone())]), false).is_err());
    schema.params.get_mut(0).unwrap().default = Some(deep);
    assert!(schema.validate().is_err());
    let schema = ParamSchema {
        params: vec![
            spec("one", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, Some(Value::Text("x".repeat(1024 * 1024)))),
            spec("two", ParamControl::Text { multiline: true, max_bytes: 10 }, None),
        ],
    };
    assert!(schema.resolve(&ParamValues::from([("two".into(), Value::Text("y".into()))]), false).is_err());
    assert!(schema.resolve(&ParamValues::from([("unknown".into(), Value::Text("x".into()))]), false).is_err());
}

#[test]
fn numeric_controls_support_unbounded_ranges_full_width_unsigned_and_exact_steps() {
    let controls = [
        (ParamControl::Slider { min: None, max: None, step: number(0.1) }, Value::Number(number(f64::MAX)), None),
        (
            ParamControl::Integer { min: Some(Integer(i64::MIN)), max: Some(Integer(i64::MAX)), step: Unsigned(u64::MAX) },
            Value::Integer(Integer(i64::MAX)),
            Some(Value::Integer(Integer(0))),
        ),
        (ParamControl::Integer { min: None, max: None, step: Unsigned(8) }, Value::Integer(Integer(-16)), Some(Value::Integer(Integer(-15)))),
        (
            ParamControl::Unsigned { min: Some(Unsigned(0)), max: Some(Unsigned(u64::MAX)), step: Unsigned(3) },
            Value::Unsigned(Unsigned(u64::MAX)),
            Some(Value::Unsigned(Unsigned(u64::MAX - 1))),
        ),
        (
            ParamControl::Seed { max: Some(Unsigned(u64::from(u32::MAX))) },
            Value::Unsigned(Unsigned(u64::from(u32::MAX))),
            Some(Value::Unsigned(Unsigned(u64::from(u32::MAX) + 1))),
        ),
    ];
    for (control, good, bad) in controls {
        let schema = ParamSchema { params: vec![spec("value", control.clone(), Some(good.clone()))] };
        assert_eq!(schema.resolve(&ParamValues::new(), true).unwrap().values.get("value"), Some(&good));
        assert_eq!(serde_json::from_str::<ParamSchema>(&serde_json::to_string(&schema).unwrap()).unwrap(), schema);
        if let Some(bad) = bad {
            assert!(control.validate_value(&bad).is_err());
            assert!(ParamSchema { params: vec![spec("value", control, Some(bad))] }.validate().is_err());
        }
    }
    for control in [
        ParamControl::Integer { min: None, max: None, step: Unsigned(0) },
        ParamControl::Unsigned { min: None, max: None, step: Unsigned(0) },
        ParamControl::Unsigned { min: Some(Unsigned(2)), max: Some(Unsigned(1)), step: Unsigned(1) },
    ] {
        assert!(ParamSchema { params: vec![spec("value", control, None)] }.validate().is_err());
    }
}

#[test]
fn large_model_choices_are_allowed_but_all_schema_text_is_budgeted() {
    let options: Vec<_> =
        (0..1000).map(|index| Choice { value: format!("models/checkpoint-{index}.safetensors"), label: format!("Checkpoint {index}") }).collect();
    let schema = ParamSchema { params: vec![spec("model", ParamControl::Choice { options }, Some(Value::Text("models/checkpoint-999.safetensors".into())))] };
    schema.resolve(&ParamValues::new(), true).unwrap();
    let oversized: Vec<_> = (0..512).map(|index| Choice { value: format!("{index}{}", "x".repeat(2048)), label: "y".repeat(2048) }).collect();
    assert!(ParamSchema { params: vec![spec("model", ParamControl::Choice { options: oversized }, None)] }.validate().is_err());
    let mut description = spec("name", ParamControl::Toggle, None);
    description.description = Some("Tooltip for generated UI and MCP".into());
    let schema = ParamSchema { params: vec![description.clone()] };
    assert_eq!(serde_json::from_str::<ParamSchema>(&serde_json::to_string(&schema).unwrap()).unwrap(), schema);
    description.label = "   ".into();
    assert!(ParamSchema { params: vec![description.clone()] }.validate().is_err());
    description.label = "Label".into();
    description.description = Some("x".repeat(4097));
    assert!(ParamSchema { params: vec![description] }.validate().is_err());
    let descriptions = (0..256)
        .map(|index| {
            let mut spec = spec(&format!("p{index}"), ParamControl::Toggle, None);
            spec.description = Some("x".repeat(4096));
            spec
        })
        .collect();
    assert!(ParamSchema { params: descriptions }.validate().is_err());
}

#[test]
fn visible_values_exclude_hidden_overrides_and_unknown_keys_have_safe_context() {
    let mut child = spec("child", ParamControl::Toggle, Some(Value::Bool(true)));
    child.visible_if = Some(VisibleIf { parameter: "enabled".into(), equals: Value::Bool(true) });
    let schema = ParamSchema { params: vec![spec("enabled", ParamControl::Toggle, Some(Value::Bool(false))), child] };
    let resolved = schema.resolve(&ParamValues::new(), true).unwrap();
    assert_eq!(resolved.values.len(), 2);
    assert_eq!(resolved.visible_values().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), vec!["enabled"]);
    assert_eq!(resolved.into_visible_values().len(), 1);
    let error = schema.resolve(&ParamValues::from([("renamed_field".into(), Value::Bool(true))]), false).unwrap_err();
    assert!(error.message.contains("renamed_field"));
    let error = schema.resolve(&ParamValues::from([("secret\nvalue".into(), Value::Bool(true))]), false).unwrap_err();
    assert!(!error.message.contains("secret"));
}

#[test]
fn aggregate_text_budget_includes_media_mime_defaults_conditions_and_overrides() {
    let mime = format!("image/png;note={}", "x".repeat(1000));
    let media = asset(&mime);
    let mut params = vec![spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, Some(Value::Text("x".repeat(900 * 1024))))];
    for index in 0..200 {
        params.push(spec(&format!("asset{index}"), ParamControl::Asset { modality: Modality::Image }, Some(media.clone())));
    }
    assert!(ParamSchema { params }.validate().is_err());
    let mut params = vec![spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, None)];
    let mut supplied = ParamValues::from([("text".into(), Value::Text("x".repeat(900 * 1024)))]);
    for index in 0..200 {
        let name = format!("asset{index}");
        params.push(spec(&name, ParamControl::Asset { modality: Modality::Image }, None));
        supplied.insert(name, media.clone());
    }
    let schema = ParamSchema { params };
    schema.validate().unwrap();
    assert!(schema.resolve(&supplied, false).is_err());
    let mut params = vec![
        spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, Some(Value::Text("x".repeat(900 * 1024)))),
        spec("controller", ParamControl::Asset { modality: Modality::Image }, None),
    ];
    for index in 0..200 {
        let mut child = spec(&format!("child{index}"), ParamControl::Toggle, None);
        child.visible_if = Some(VisibleIf { parameter: "controller".into(), equals: media.clone() });
        params.push(child);
    }
    assert!(ParamSchema { params }.validate().is_err());
}

#[test]
fn borrowed_resolution_preserves_hidden_checks_and_effective_byte_boundaries() {
    let mut hidden =
        spec("hidden", ParamControl::Choice { options: vec![Choice { value: "yes".into(), label: "Yes".into() }] }, Some(Value::Text("yes".into())));
    hidden.visible_if = Some(VisibleIf { parameter: "toggle".into(), equals: Value::Bool(true) });
    let schema = ParamSchema { params: vec![spec("toggle", ParamControl::Toggle, Some(Value::Bool(false))), hidden] };
    let validated = schema.validated().unwrap();
    let supplied = ParamValues::from([("hidden".into(), Value::Text("yes".into()))]);
    let resolved = validated.resolve_borrowed(&supplied, true).unwrap();
    assert!(!resolved.visible.contains("hidden"));
    assert_eq!(resolved.values.get("hidden").copied(), supplied.get("hidden"));
    let invalid = ParamValues::from([("hidden".into(), Value::Text("no".into()))]);
    assert!(validated.resolve_borrowed(&invalid, false).is_err(), "hidden supplied values are still checked");
    let schema = ParamSchema {
        params: vec![
            spec("a", ParamControl::Text { multiline: true, max_bytes: 1_048_576 }, Some(Value::Text("a".repeat(100)))),
            spec("b", ParamControl::Text { multiline: true, max_bytes: 1_048_576 }, None),
        ],
    };
    let validated = schema.validated().unwrap();
    for (length, succeeds) in [(1_048_476, true), (1_048_477, false)] {
        let supplied = ParamValues::from([("b".into(), Value::Text("b".repeat(length)))]);
        assert_eq!(validated.resolve_borrowed(&supplied, false).is_ok(), succeeds);
        assert_eq!(schema.resolve(&supplied, false).is_ok(), succeeds);
    }
}
