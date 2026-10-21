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
    assert!(kind.mode(&[PortType { modality: Modality::Text, many: false }]).is_none());
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
    let many = Timeline { clips: (0..=MAX_CLIPS).map(|i| Clip::whole(format!("c{i}"), "n".into(), 0)).collect(), tracks: vec![], ..Timeline::default() };
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
        ..Timeline::default()
    };
    let sources = [Wire::video("a"), Wire::video("b"), Wire::video("a"), Wire::video("")];
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

#[test]
fn tracks_hold_sounds_and_pictures_at_their_own_time() {
    let wires = [Wire::video("v"), Wire::new(Input::Audio, "m"), Wire::new(Input::Stills, "p"), Wire::new(Input::Stills, "p")];
    let shown = Timeline::default().with_sources(&wires);
    let placed: Vec<(&str, Input, u8, Option<u64>)> = shown.clips.iter().map(|c| (c.id.as_str(), c.input, c.track, c.out_us)).collect();
    // Videos and stills join the main track; the sound gets an audio track of its own.
    assert_eq!(
        placed,
        [
            ("v-0", Input::Clips, 0, None),
            ("m-a0", Input::Audio, 1, None),
            ("p-s0", Input::Stills, 0, Some(DEFAULT_STILL_US)),
            ("p-s1", Input::Stills, 0, Some(DEFAULT_STILL_US))
        ]
    );
    assert_eq!(shown.tracks, [Track { kind: TrackKind::Audio }]);
    assert!(shown.check().is_ok());
    assert_eq!(Timeline::parse(&shown.encode().unwrap()).unwrap(), shown);

    // A still laid over the picture on a video track, after the audio track.
    let mut layered = shown.clone();
    layered.tracks.push(Track { kind: TrackKind::Video });
    layered.clips.retain(|c| c.id != "p-s1");
    layered.clips.insert(0, Clip { track: 2, at_us: 500_000, ..Clip::wired("logo".into(), Input::Stills, "p".into(), 1, 2) });
    assert!(layered.check().is_ok());
    let resolved = layered.resolve(&wires);
    let order: Vec<(usize, &str)> = resolved.iter().map(|(i, c)| (*i, c.id.as_str())).collect();
    // The main track first, in order, then each track in turn; "p-s1" is named by "logo".
    assert_eq!(order, [(0, "v-0"), (2, "p-s0"), (1, "m-a0"), (3, "logo")]);

    for bad in [
        // A sound on the main track, a picture on an audio track, a missing track.
        r#"{"clips":[{"id":"a","origin":"n","input":"audio"}]}"#,
        r#"{"clips":[{"id":"a","origin":"n","track":1}],"tracks":[{"kind":"audio"}]}"#,
        r#"{"clips":[{"id":"a","origin":"n","track":1}]}"#,
        // A placed clip on the main track, a transition off it, a still with no length.
        r#"{"clips":[{"id":"a","origin":"n","at_us":5}]}"#,
        r#"{"clips":[{"id":"a","origin":"n","track":1,"transition_us":100000,"out_us":1000000}],"tracks":[{"kind":"video"}]}"#,
        r#"{"clips":[{"id":"a","origin":"n","input":"stills"}]}"#,
        r#"{"clips":[],"tracks":[{"kind":"subtitle"}]}"#,
    ] {
        assert!(Timeline::parse(bad).is_err(), "{bad}");
    }
    let crowded = Timeline { clips: vec![], tracks: vec![Track { kind: TrackKind::Video }; MAX_TRACKS + 1], ..Timeline::default() };
    assert!(crowded.check().is_err());
    // With every track a video track, a sound has nowhere to go and isn't shown.
    let full = Timeline { clips: vec![], tracks: vec![Track { kind: TrackKind::Video }; MAX_TRACKS], ..Timeline::default() };
    assert!(full.with_sources(&[Wire::new(Input::Audio, "m")]).clips.is_empty());
    // ... which the run refuses, rather than drop the sound unheard.
    assert!(full.unplaced_sound(&[Wire::video("v"), Wire::new(Input::Audio, "m")]));
    assert!(!full.unplaced_sound(&[Wire::video("v"), Wire::new(Input::Stills, "p")]));
    assert!(!Timeline::default().unplaced_sound(&[Wire::new(Input::Audio, "m")]));
}

#[test]
fn its_plan_names_each_inputs_port() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(ID).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), Default::default(), &ParamValues::default()).unwrap();
    assert!(kind.mode(&[PortType { modality: Modality::Audio, many: false }, PortType { modality: Modality::Image, many: false }]).is_some());
    let (a, b) = (NodeId::from_bytes([1; 16]), NodeId::from_bytes([2; 16]));
    let sound = InputValue {
        port: AUDIO_INPUT.into(),
        port_type: PortType { modality: Modality::Audio, many: true },
        value: Value::Text("s".into()),
        origins: vec![a],
    };
    let still = InputValue {
        port: STILLS_INPUT.into(),
        port_type: PortType { modality: Modality::Image, many: true },
        value: Value::Text("p".into()),
        origins: vec![b],
    };
    let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[sound, still], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
    let [JobSpec::Action { params, inputs, .. }] = jobs.as_slice() else { panic!("{jobs:?}") };
    assert_eq!(inputs.len(), 2);
    assert_eq!(params.get(PORTS_PARAM), Some(&Value::List(vec![Value::Text(AUDIO_INPUT.into()), Value::Text(STILLS_INPUT.into())])));
}

#[test]
fn a_card_made_before_sound_and_picture_inputs_still_plans_and_migrates() {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(ID).unwrap();
    let mut node = kind.instantiate_instance(NodeId::from_bytes([5; 16]), Default::default(), &ParamValues::default()).unwrap();
    node.inputs.truncate(1);
    let input = InputValue { port: INPUT.into(), port_type: video(), value: Value::Text("x".into()), origins: vec![NodeId::from_bytes([1; 16])] };
    assert_eq!(kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap().len(), 1);
    let migrated = kind.prepare_migration(&node).unwrap().unwrap().node;
    assert_eq!(migrated.inputs, kind.descriptor().inputs);
    assert!(kind.prepare_migration(&migrated).unwrap().is_none());
    // Ports that are not a prefix of today's are still refused.
    node.inputs = vec![kind.descriptor().inputs[1].clone()];
    assert!(kind.validate_instance(&node, false).is_err());
}

#[test]
fn captions_are_stored_with_the_clips_and_checked() {
    // A timeline stored before captions reads, and stores, as it did.
    let old =
        r#"{"clips":[{"id":"c","origin":"n","nth":0,"in_us":0,"fade_in_us":0,"fade_out_us":0,"volume":1000,"muted":false,"transition_us":0,"enabled":true}]}"#;
    let parsed = Timeline::parse(old).unwrap();
    assert!(parsed.captions.is_empty());
    assert_eq!(parsed.encode().unwrap(), old);

    let caption = Caption::new("hello".into(), "Hello\nworld".into(), 500_000, 2_000_000);
    let with = Timeline { captions: vec![caption.clone()], ..parsed.clone() };
    let json = with.encode().unwrap();
    assert!(json.contains(r##""captions":[{"id":"hello","text":"Hello\nworld","start_us":500000,"end_us":2000000,"font":"lato","size":48,"align":"center","position":"bottom","color":"#ffffff","stroke":"#000000","stroke_width":0,"enabled":true}]"##), "{json}");
    assert_eq!(Timeline::parse(&json).unwrap(), with);
    // Fields left out take their defaults.
    let short = r#"{"clips":[],"captions":[{"id":"a","text":"Hi","start_us":0,"end_us":1}]}"#;
    assert_eq!(Timeline::parse(short).unwrap().captions, vec![Caption::new("a".into(), "Hi".into(), 0, 1)]);

    let bad = [
        Caption { id: "".into(), ..caption.clone() },
        Caption { text: "  ".into(), ..caption.clone() },
        Caption { text: "x".repeat(caption::MAX_CAPTION_BYTES + 1), ..caption.clone() },
        Caption { text: "a\tb".into(), ..caption.clone() },
        Caption { text: "a\n".repeat(caption::MAX_CAPTION_LINES), ..caption.clone() },
        Caption { end_us: 500_000, ..caption.clone() },
        Caption { end_us: MAX_US + 1, ..caption.clone() },
        Caption { font: "".into(), ..caption.clone() },
        Caption { font: "f".repeat(caption::MAX_FONT_BYTES + 1), ..caption.clone() },
        Caption { size: caption::MIN_CAPTION_SIZE - 1, ..caption.clone() },
        Caption { size: caption::MAX_CAPTION_SIZE + 1, ..caption.clone() },
        Caption { color: "white".into(), ..caption.clone() },
        Caption { stroke: "#12345".into(), ..caption.clone() },
        Caption { stroke: "#gggggg".into(), ..caption.clone() },
        Caption { stroke_width: caption::MAX_CAPTION_STROKE + 1, ..caption.clone() },
    ];
    for b in bad {
        assert!(Timeline { captions: vec![b.clone()], ..Timeline::default() }.check().is_err(), "{b:?}");
    }
    assert!(Caption { text: "a\n".repeat(caption::MAX_CAPTION_LINES - 1), color: "#11223344".into(), ..caption.clone() }.check().is_ok());
    let twice = Timeline { captions: vec![caption.clone(), caption.clone()], ..Timeline::default() };
    assert_eq!(twice.check().unwrap_err().kind, ErrorKind::Conflict);
    let many = Timeline { captions: (0..=MAX_CAPTIONS).map(|i| Caption::new(format!("c{i}"), "x".into(), 0, 1)).collect(), ..Timeline::default() };
    assert!(many.check().is_err());
    // The most captions, each as long as it may be, still store.
    let full = Timeline {
        captions: (0..MAX_CAPTIONS).map(|i| Caption::new(format!("c{i}"), "\"".repeat(caption::MAX_CAPTION_BYTES), 0, 1)).collect(),
        ..Timeline::default()
    };
    assert!(full.encode().is_ok());
    assert_eq!(Caption::fresh_id(&with.captions), "caption-1");
    let taken = [Caption::new("caption-1".into(), "x".into(), 0, 1)];
    assert_eq!(Caption::fresh_id(&taken), "caption-2");
}
