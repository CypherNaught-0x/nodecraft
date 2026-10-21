use super::*;
use crate::timeline::{Clip, Track};

fn source(name: &str, file: &str, kind: MediaKind, seconds: u64) -> Source {
    let duration_us = (kind != MediaKind::Still).then_some(seconds * 1_000_000);
    Source { name: name.into(), file: file.into(), kind, sound: kind != MediaKind::Still, duration_us }
}

fn sequence<'a>(timeline: &'a Timeline, wires: &'a [Wire], media: &'a [Option<usize>], sources: &'a [Source]) -> Sequence<'a> {
    Sequence { name: "Cut", width: 1280, height: 720, fps: 30, background: [0, 0, 0], timeline, wires, media, sources }
}

/// A fox (4 s) crossing over a second into an owl (3 s), both videos with sound.
fn crossing() -> (Timeline, Vec<Wire>, Vec<Source>) {
    let mut fox = Clip::wired("fox".into(), Input::Clips, "n1".into(), 0, 0);
    fox.transition_us = 1_000_000;
    let owl = Clip::wired("owl".into(), Input::Clips, "n2".into(), 0, 0);
    let sources = vec![source("Fox", "media/01-fox.mp4", MediaKind::Video, 4), source("Owl", "media/02-owl.mp4", MediaKind::Video, 3)];
    (Timeline { clips: vec![fox, owl], tracks: Vec::new() }, vec![Wire::video("n1"), Wire::video("n2")], sources)
}

#[test]
fn a_cross_transition_is_a_dissolve_centred_on_the_cut() {
    let (timeline, wires, sources) = crossing();
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap();
    // 4 s + 3 s overlapping by 1 s is 6 s, as a render plays it.
    assert_eq!(out.frames, 180);
    assert!(out.xml.contains(r#"<asset-clip ref="r2" name="fox" offset="0s" start="0s" duration="105/30s"/>"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<transition name="Cross Dissolve" offset="90/30s" duration="30/30s"/>"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<asset-clip ref="r3" name="owl" offset="105/30s" start="15/30s" duration="75/30s"/>"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<sequence format="r1" duration="180/30s""#));
    assert!(out.xml.contains(r#"<format id="r1" frameDuration="1/30s" width="1280" height="720"/>"#));
    assert!(out.xml.contains(r#"<media-rep kind="original-media" src="media/01-fox.mp4"/>"#));
    assert!(out.losses.is_empty(), "{:?}", out.losses);
    assert_eq!(out.used, [0, 1]);
}

#[test]
fn other_tracks_connect_to_what_is_under_them_on_lanes() {
    let (mut timeline, mut wires, mut sources) = crossing();
    timeline.tracks = vec![Track { kind: TrackKind::Video }, Track { kind: TrackKind::Audio }];
    // A still over the end of the cut, past the main track, and a sound under the fox.
    let mut still = Clip::wired("logo".into(), Input::Stills, "n3".into(), 0, 1);
    still.at_us = 5_000_000;
    still.out_us = Some(2_000_000);
    let mut sound = Clip::wired("birds".into(), Input::Audio, "n4".into(), 0, 2);
    sound.at_us = 1_000_000;
    sound.in_us = 500_000;
    sound.out_us = Some(2_500_000);
    sound.volume = 500;
    timeline.clips.extend([still, sound]);
    wires.extend([Wire::new(Input::Stills, "n3"), Wire::new(Input::Audio, "n4")]);
    sources.extend([source("Logo", "media/03-logo.png", MediaKind::Still, 0), source("Birds", "media/04-birds.wav", MediaKind::Audio, 9)]);
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1), Some(2), Some(3)], &sources)).unwrap();
    // The still ends at 7 s, a second after the main track: a gap runs the sequence on.
    assert_eq!(out.frames, 210);
    assert!(out.xml.contains(r#"<gap name="Gap" offset="180/30s" start="180/30s" duration="30/30s"/>"#), "{}", out.xml);
    // It starts at 5 s over the owl, which is at its own frame 15 at sequence frame 105.
    assert!(out.xml.contains(r#"<video ref="r4" lane="1" name="logo" offset="60/30s" start="0s" duration="60/30s"/>"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<asset-clip ref="r5" lane="-1" name="birds" offset="30/30s" start="15/30s" duration="60/30s">"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<adjust-volume amount="-6.02dB"/>"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<asset id="r4" name="Logo" start="0s" duration="0s" hasVideo="1" hasAudio="0">"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<asset id="r5" name="Birds" start="0s" duration="9000000/1000000s" hasVideo="0" hasAudio="1">"#), "{}", out.xml);
}

#[test]
fn a_connected_clip_over_a_later_main_clip_uses_its_media_time() {
    let (mut timeline, mut wires, mut sources) = crossing();
    timeline.tracks = vec![Track { kind: TrackKind::Audio }];
    let mut sound = Clip::wired("birds".into(), Input::Audio, "n4".into(), 0, 1);
    sound.at_us = 4_000_000;
    sound.out_us = Some(1_000_000);
    timeline.clips.push(sound);
    wires.push(Wire::new(Input::Audio, "n4"));
    sources.push(source("Birds", "media/03-birds.wav", MediaKind::Audio, 9));
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1), Some(2)], &sources)).unwrap();
    // At 4 s (frame 120) the owl is on the main track: it starts at 105 from its frame 15.
    assert!(out.xml.contains(r#"<asset-clip ref="r4" lane="-1" name="birds" offset="30/30s" start="0s" duration="30/30s"/>"#), "{}", out.xml);
}

#[test]
fn a_clip_after_the_main_track_connects_to_a_gap() {
    let (mut timeline, mut wires, mut sources) = crossing();
    timeline.tracks = vec![Track { kind: TrackKind::Video }];
    let mut still = Clip::wired("logo".into(), Input::Stills, "n3".into(), 0, 1);
    still.at_us = 8_000_000;
    timeline.clips.push(still);
    wires.push(Wire::new(Input::Stills, "n3"));
    sources.push(source("Logo", "media/03-logo.png", MediaKind::Still, 0));
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1), Some(2)], &sources)).unwrap();
    assert_eq!(out.frames, 330, "8 s plus the still's 3 s");
    assert!(out.xml.contains(r#"<gap name="Gap" offset="180/30s" start="180/30s" duration="150/30s">"#), "{}", out.xml);
    assert!(out.xml.contains(r#"<video ref="r4" lane="1" name="logo" offset="240/30s" start="0s" duration="90/30s"/>"#), "{}", out.xml);
}

#[test]
fn media_played_twice_is_one_asset() {
    let first = Clip::wired("a".into(), Input::Clips, "n1".into(), 0, 0);
    let mut again = Clip::wired("b".into(), Input::Clips, "n1".into(), 0, 0);
    again.in_us = 1_000_000;
    // Two wires from two nodes that made the same file.
    let wires = [Wire::video("n1"), Wire::video("n2")];
    let mut copy = Clip::wired("c".into(), Input::Clips, "n2".into(), 0, 0);
    copy.out_us = Some(1_000_000);
    let timeline = Timeline { clips: vec![first, again, copy], tracks: Vec::new() };
    let sources = [source("Fox", "media/01-fox.mp4", MediaKind::Video, 4)];
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(0)], &sources)).unwrap();
    assert_eq!(out.used, [0]);
    assert_eq!(out.xml.matches("<asset ").count(), 1);
    assert_eq!(out.xml.matches(r#"ref="r2""#).count(), 3);
    assert_eq!(out.frames, 120 + 90 + 30);
}

#[test]
fn names_are_escaped_and_paths_percent_encoded() {
    let clip = Clip::wired("fox".into(), Input::Clips, "n1".into(), 0, 0);
    let timeline = Timeline { clips: vec![clip], tracks: Vec::new() };
    let sources = [source("Füchse & <\"Eulen\">\u{7}", "media/01-füchse eulen.mp4", MediaKind::Video, 2)];
    let mut seq = sequence(&timeline, &[], &[Some(0)], &sources);
    let wires = [Wire::video("n1")];
    seq.wires = &wires;
    seq.name = "Schnitt 'eins'";
    let out = fcpxml(&seq).unwrap();
    assert!(out.xml.contains(r#"name="Füchse &amp; &lt;&quot;Eulen&quot;&gt; ""#), "{}", out.xml);
    assert!(out.xml.contains(r#"src="media/01-f%C3%BCchse%20eulen.mp4""#), "{}", out.xml);
    assert!(out.xml.contains(r#"<project name="Schnitt &apos;eins&apos;">"#));
}

#[test]
fn what_the_document_cannot_carry_is_listed() {
    let (mut timeline, wires, sources) = crossing();
    if let Some(fox) = timeline.clips.first_mut() {
        fox.fade_in_us = 500_000;
        fox.in_us = 10_000; // a third of a frame at 30 fps
        fox.muted = true;
    }
    timeline.clips.push(Clip { enabled: false, ..Clip::wired("off".into(), Input::Clips, "n2".into(), 0, 0) });
    timeline.clips.push(Clip::wired("gone".into(), Input::Clips, "n9".into(), 0, 0));
    let mut seq = sequence(&timeline, &wires, &[Some(0), Some(1)], &sources);
    seq.background = [255, 255, 255];
    let out = fcpxml(&seq).unwrap();
    let features: Vec<(&str, Option<&str>)> = out.losses.iter().map(|l| (l.feature, l.clip.as_deref())).collect();
    for expected in [
        ("disabled clip", Some("off")),
        ("unconnected clip", Some("gone")),
        ("sub-frame timing", Some("fox")),
        ("fade", Some("fox")),
        ("background colour", None),
    ] {
        assert!(features.contains(&expected), "{expected:?} in {features:?}");
    }
    assert!(out.xml.contains(r#"<adjust-volume amount="-96.00dB"/>"#), "a muted clip is silent: {}", out.xml);
}

#[test]
fn clips_that_cannot_be_placed_are_refused_with_their_number() {
    let (timeline, wires, mut sources) = crossing();
    let missing = fcpxml(&sequence(&timeline, &wires, &[Some(0), None], &sources)).unwrap_err();
    assert!(missing.message.contains("clip 2 has no media yet"), "{}", missing.message);
    sources[1].duration_us = None;
    let unknown = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap_err();
    assert!(unknown.message.contains("clip 2's media doesn't say how long"), "{}", unknown.message);
    let (mut timeline, wires, sources) = crossing();
    if let Some(owl) = timeline.clips.get_mut(1) {
        owl.out_us = Some(5_000_000);
    }
    let long = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap_err();
    assert!(long.message.contains("clip 2 ends after its media"), "{}", long.message);
    let empty = Timeline::default();
    assert!(fcpxml(&sequence(&empty, &[], &[], &[])).is_err(), "nothing to export");
    let mut seq = sequence(&timeline, &wires, &[Some(0), Some(1)], &sources);
    seq.fps = 0;
    assert!(fcpxml(&seq).is_err());
}

#[test]
fn a_transition_longer_than_a_clip_allows_is_refused() {
    let (mut timeline, wires, mut sources) = crossing();
    sources[1].duration_us = Some(400_000);
    if let Some(fox) = timeline.clips.first_mut() {
        fox.transition_us = 900_000;
    }
    let error = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap_err();
    assert!(error.message.contains("shorter than its transitions"), "{}", error.message);
}

#[test]
fn every_rate_the_timeline_offers_lands_on_whole_frames() {
    let (timeline, wires, sources) = crossing();
    for rate in crate::timeline::RATES {
        let fps: u32 = rate.parse().unwrap();
        let mut seq = sequence(&timeline, &wires, &[Some(0), Some(1)], &sources);
        seq.fps = fps;
        let out = fcpxml(&seq).unwrap();
        assert_eq!(out.frames, u64::from(fps) * 6, "{rate} fps");
        assert!(out.losses.is_empty(), "{rate}: {:?}", out.losses);
    }
}

#[test]
fn the_document_is_balanced() {
    let (mut timeline, mut wires, mut sources) = crossing();
    timeline.tracks = vec![Track { kind: TrackKind::Audio }];
    let mut sound = Clip::wired("birds".into(), Input::Audio, "n4".into(), 0, 1);
    sound.volume = 2000;
    timeline.clips.push(sound);
    wires.push(Wire::new(Input::Audio, "n4"));
    sources.push(source("Birds", "media/03-birds.wav", MediaKind::Audio, 2));
    let xml = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1), Some(2)], &sources)).unwrap().xml;
    // A small check of nesting: every open tag closes, in order.
    let mut open: Vec<String> = Vec::new();
    for tag in xml.split('<').skip(1).map(|t| t.split('>').next().unwrap()) {
        if tag.starts_with('?') || tag.starts_with('!') || tag.ends_with('/') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            assert_eq!(open.pop().as_deref(), Some(name), "{xml}");
        } else {
            open.push(tag.split(' ').next().unwrap().to_owned());
        }
    }
    assert!(open.is_empty(), "{open:?}");
    assert!(xml.contains(r#"amount="6.02dB""#), "{xml}");
}

#[test]
fn urls_keep_unreserved_bytes_and_slashes() {
    assert_eq!(relative_url("media/01-a_b.c~d.mp4"), "media/01-a_b.c~d.mp4");
    assert_eq!(relative_url("media/a b#?.mp4"), "media/a%20b%23%3F.mp4");
}

#[test]
fn a_sound_with_no_stored_clip_plays_on_the_sound_track_it_is_given() {
    // No stored clips and no tracks: the sound gets the audio track `with_sources` adds.
    let timeline = Timeline { clips: Vec::new(), tracks: Vec::new() };
    let wires = vec![Wire::video("n1"), Wire::new(Input::Audio, "n2".to_owned())];
    let sources = vec![source("Fox", "media/01-fox.mp4", MediaKind::Video, 4), source("Song", "media/02-song.wav", MediaKind::Audio, 6)];
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap();
    assert!(out.xml.contains("lane=\"-1\""), "{}", out.xml);
    assert_eq!(out.used, [0, 1]);
}

#[test]
fn rounding_never_runs_a_clip_past_the_end_of_its_media() {
    let (mut timeline, wires, mut sources) = crossing();
    if let Some(fox) = timeline.clips.first_mut() {
        fox.transition_us = 0;
    }
    // 2.99 s is 89.7 frames at 30 fps: the nearest frame, 90, is past the last one the owl has.
    sources[1].duration_us = Some(2_990_000);
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap();
    assert!(out.xml.contains(r#"<asset-clip ref="r3" name="owl" offset="120/30s" start="0s" duration="89/30s"/>"#), "{}", out.xml);
    assert_eq!(out.frames, 209);
}

#[test]
fn media_of_unknown_length_is_as_long_as_its_clips_play_it() {
    let (mut timeline, wires, mut sources) = crossing();
    sources[1].duration_us = None;
    if let Some(owl) = timeline.clips.get_mut(1) {
        owl.out_us = Some(2_000_000);
    }
    let out = fcpxml(&sequence(&timeline, &wires, &[Some(0), Some(1)], &sources)).unwrap();
    assert!(out.xml.contains(r#"name="Owl" start="0s" duration="60/30s""#), "{}", out.xml);
    assert!(out.losses.iter().any(|loss| loss.feature == "unknown media length" && loss.clip.is_none()), "{:?}", out.losses);
}

#[test]
fn characters_xml_cannot_carry_become_spaces() {
    assert_eq!(escape("a\u{FFFE}b\u{FFFF}c\u{1}d"), "a b c d");
}
