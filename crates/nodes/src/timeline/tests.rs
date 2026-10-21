use super::*;
use crate::{InputValue, Registry};
use nodecraft_types::{NodeId, ParamValues};

fn video() -> PortType {
    PortType { modality: Modality::Video, many: true }
}

#[test]
fn its_plan_carries_the_clips_and_where_each_video_came_from() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(ID).unwrap();
    let mut node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), Default::default(), &ParamValues::default()).unwrap();
    assert_eq!(stored(&node).unwrap(), EMPTY);
    assert!(kind.mode(&[video()]).is_some() && kind.mode(&[]).is_some());
    assert!(kind.mode(&[PortType { modality: Modality::Image, many: false }]).is_none());
    let (a, b) = (NodeId::from_bytes([1; 16]), NodeId::from_bytes([2; 16]));
    let input =
        InputValue { port: INPUT.into(), port_type: video(), value: Value::List(vec![Value::Text("x".into()), Value::Text("y".into())]), origins: vec![a, b] };
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
    let [JobSpec::Action { action, params, inputs, output, .. }] = jobs.as_slice() else { panic!("{jobs:?}") };
    assert_eq!((action.as_str(), output.as_str(), inputs.len()), (ID, OUTPUT, 2));
    assert_eq!(params.get(SPEC_PARAM), Some(&Value::Text(EMPTY.into())));
    assert_eq!(params.get(SOURCES_PARAM), Some(&Value::List(vec![Value::Text(a.to_string()), Value::Text(b.to_string())])));
    assert_eq!(params.get("width"), Some(&Value::Unsigned(Unsigned(DEFAULT_WIDTH))));
    assert_eq!(params.get("fps"), Some(&Value::Text("30".into())));

    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err());
    node.config.insert(CONFIG.into(), Value::Bool(true));
    let input = InputValue { port: INPUT.into(), port_type: video(), value: Value::Text("x".into()), origins: vec![a] };
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err());
}

#[test]
fn clips_are_checked_where_they_are_read() {
    let ok = r#"{"clips":[{"id":"a","origin":"n1","in_us":1000,"out_us":2000000,"fade_in_us":500000,"volume":1500,"transition_us":250000}]}"#;
    let timeline = Timeline::parse(ok).unwrap();
    let clip = timeline.get("a").unwrap();
    assert_eq!((clip.length_us(), clip.enabled, clip.muted, clip.nth), (Some(1_999_000), true, false, 0));
    assert_eq!(Timeline::parse(&timeline.encode().unwrap()).unwrap(), timeline);
    for bad in [
        r#"{"clips":[{"id":"a","origin":"n1","out_us":0}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1","in_us":5,"out_us":5}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1","out_us":1000,"fade_in_us":600,"fade_out_us":600}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1","transition_us":9999}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1","volume":4001}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1","in_us":3600000001}]}"#,
        r#"{"clips":[{"id":"","origin":"n1"}]}"#,
        r#"{"clips":[{"id":"a b","origin":"n1"}]}"#,
        r#"{"clips":[{"id":"a","origin":"../x"}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1"},{"id":"a","origin":"n2"}]}"#,
        r#"{"clips":[{"id":"a","origin":"n1","speed":2}]}"#,
        r#"{"clips":{}}"#,
    ] {
        assert!(Timeline::parse(bad).is_err(), "{bad}");
    }
    let many = Timeline { clips: (0..=MAX_CLIPS).map(|i| Clip::whole(format!("c{i}"), "n".into(), 0)).collect() };
    assert!(many.encode().is_err());
    assert!(Timeline::parse(&" ".repeat(MAX_ENCODED_BYTES + 1)).is_err());
}

#[test]
fn wired_videos_get_whole_clips_and_resolve_to_their_inputs() {
    let stored = Timeline {
        clips: vec![
            Clip { out_us: Some(1_000_000), transition_us: 100_000, ..Clip::whole("cut".into(), "b".into(), 0) },
            Clip { enabled: false, ..Clip::whole("off".into(), "a".into(), 1) },
            Clip::whole("gone".into(), "z".into(), 0),
            // A taken automatic id makes the automatic clip pick another.
            Clip { transition_us: 50_000, ..Clip::whole("a-0".into(), "b".into(), 0) },
        ],
    };
    let sources = ["a".to_owned(), "b".into(), "a".into(), String::new()];
    let shown = stored.with_sources(&sources);
    let ids: Vec<&str> = shown.clips.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["cut", "off", "gone", "a-0", "a-0-2", "input3-0"]);
    assert!(shown.check().is_ok());
    let resolved = stored.resolve(&sources);
    let order: Vec<(usize, &str)> = resolved.iter().map(|(input, c)| (*input, c.id.as_str())).collect();
    // The second video from "a" is disabled, "z" isn't wired, and the same video plays twice.
    assert_eq!(order, [(1, "cut"), (1, "a-0"), (0, "a-0-2"), (3, "input3-0")]);
    assert_eq!(resolved.first().map(|(_, c)| c.transition_us), Some(100_000));
    assert_eq!(resolved.last().map(|(_, c)| c.transition_us), Some(0));
    assert!(Timeline::is_wired(&stored.clips[0], &sources) && !Timeline::is_wired(&stored.clips[2], &sources));
    assert!(Timeline::default().resolve(&[]).is_empty());
}
