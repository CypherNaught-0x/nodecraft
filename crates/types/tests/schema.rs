use nodecraft_types::{
    AssetRef, Blake3, Choice, Integer, MediaMeta, Modality, Number, ParamControl, ParamSchema, ParamSpec, ParamValues, Unsigned, Value, VisibleIf,
};

fn number(value: f64) -> Number {
    Number::new(value).unwrap()
}
fn spec(name: &str, control: ParamControl, default: Option<Value>) -> ParamSpec {
    ParamSpec { name: name.into(), label: name.into(), control, required: true, default, visible_if: None }
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
        (ParamControl::Slider { min: number(-1.0), max: number(1.0), step: number(0.1) }, Value::Number(number(0.25)), Value::Number(number(2.0))),
        (ParamControl::Integer { min: Integer(i64::MIN), max: Integer(100) }, Value::Integer(Integer(i64::MIN)), Value::Integer(Integer(101))),
        (ParamControl::Color, Value::Text("#aAbBcc80".into()), Value::Text("#gg0000".into())),
        (ParamControl::Model, Value::Text("provider/model-v1".into()), Value::Text("contains space".into())),
        (ParamControl::Seed, Value::Unsigned(Unsigned(u64::MAX)), Value::Integer(Integer(-1))),
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
        ParamControl::Slider { min: number(1.0), max: number(0.0), step: number(1.0) },
        ParamControl::Slider { min: number(0.0), max: number(1.0), step: number(0.0) },
        ParamControl::Integer { min: Integer(2), max: Integer(1) },
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
