use super::*;
use crate::timeline::interchange::{Sequence, Source, fcpxml, fcpxml_controlled};
use crate::timeline::{Input, Timeline, Track, TrackKind, Wire};

fn clip(input: Input, track: u8) -> Clip {
    let mut clip = Clip::wired("fade".into(), input, "source".into(), 0, track);
    clip.in_us = 1_000_000;
    clip.out_us = Some(4_000_000);
    clip.fade_in_us = 1_000_000;
    clip.fade_out_us = 500_000;
    clip.opacity = 500;
    clip.volume = 500;
    clip
}
fn export(clip: Clip, kind: MediaKind, fps: u32) -> Result<super::super::Interchange, Error> {
    let wires = [Wire::new(clip.input, "source")];
    let timeline = Timeline {
        clips: vec![clip],
        tracks: vec![Track { kind: if kind == MediaKind::Audio { TrackKind::Audio } else { TrackKind::Video } }],
        captions: vec![],
    };
    let sources = [Source {
        name: "Original".into(),
        file: "media/original.bin".into(),
        kind,
        sound: kind != MediaKind::Still,
        duration_us: (kind != MediaKind::Still).then_some(5_000_000),
    }];
    fcpxml(&Sequence {
        name: "Fades",
        width: 320,
        height: 180,
        fps,
        background: [0; 3],
        timeline: &timeline,
        wires: &wires,
        media: &[Some(0)],
        sources: &sources,
    })
}

#[test]
fn picture_golden_uses_source_clock_and_clip_opacity() {
    let out = export(clip(Input::Clips, 0), MediaKind::Video, 10).unwrap();
    let picture = out.xml.split("<adjust-blend").nth(1).unwrap().split("</adjust-blend>").next().unwrap();
    assert_eq!(
        picture,
        " amount=\"0.500000\">\n                <param name=\"amount\">\n                  <keyframeAnimation>\n                    <keyframe time=\"10/10s\" value=\"0.000000\" interp=\"linear\" curve=\"linear\"/>\n                    <keyframe time=\"20/10s\" value=\"0.500000\" interp=\"linear\" curve=\"linear\"/>\n                    <keyframe time=\"35/10s\" value=\"0.500000\" interp=\"linear\" curve=\"linear\"/>\n                    <keyframe time=\"40/10s\" value=\"0.000000\" interp=\"linear\" curve=\"linear\"/>\n                  </keyframeAnimation>\n                </param>\n              "
    );
    assert!(!out.losses.iter().any(|loss| matches!(loss.feature, "fade" | "opacity")));
}

#[test]
fn connected_sound_samples_linear_gain_before_db_conversion() {
    let mut sound = clip(Input::Audio, 1);
    sound.at_us = 2_000_000;
    let out = export(sound, MediaKind::Audio, 10).unwrap();
    assert!(!out.xml.contains("adjust-blend"));
    assert!(out.xml.contains("lane=\"-1\""));
    for key in [
        "time=\"10/10s\" value=\"-96.00dB\"",
        "time=\"15/10s\" value=\"-12.04dB\"",
        "time=\"20/10s\" value=\"-6.02dB\"",
        "time=\"35/10s\" value=\"-6.02dB\"",
        "time=\"39/10s\" value=\"-20.00dB\"",
        "time=\"40/10s\" value=\"-96.00dB\"",
    ] {
        assert!(out.xml.contains(key), "{key}: {}", out.xml);
    }
    assert_eq!(out.xml.matches("<keyframe ").count(), 17);
    assert!(out.losses.iter().any(|loss| loss.feature == "audio fade interpolation"));
}

#[test]
fn silent_and_transparent_fades_stay_silent_and_transparent() {
    let mut muted = clip(Input::Clips, 0);
    muted.muted = true;
    muted.opacity = 0;
    let out = export(muted, MediaKind::Video, 10).unwrap();
    for line in out.xml.lines().filter(|line| line.contains("<keyframe ")) {
        assert!(line.contains("value=\"0.000000\"") || line.contains("value=\"-96.00dB\""), "{line}");
    }
}

#[test]
fn still_fades_have_no_audio_and_rate_rounding_is_reported() {
    let mut still = clip(Input::Stills, 1);
    still.in_us = 0;
    still.out_us = Some(2_000_000);
    still.fade_in_us = 1;
    still.fade_out_us = 1_000_000;
    let out = export(still, MediaKind::Still, 30).unwrap();
    assert!(out.xml.contains("<adjust-blend"));
    assert!(!out.xml.contains("adjust-volume"));
    assert!(out.losses.iter().any(|loss| loss.feature == "sub-frame timing"));
}

#[test]
fn speed_maps_played_fades_into_source_time_and_keeps_retime_loss() {
    let mut fast = clip(Input::Clips, 0);
    fast.speed = 2000;
    let out = export(fast, MediaKind::Video, 10).unwrap();
    assert!(out.xml.contains("time=\"30/10s\" value=\"0.500000\""));
    assert!(out.losses.iter().any(|loss| loss.feature == "speed"));
}

#[test]
fn transition_handles_keep_the_original_envelope_boundaries() {
    let mut first = clip(Input::Clips, 0);
    first.transition_us = 1_000_000;
    let mut next = first.clone();
    next.id = "next".into();
    next.transition_us = 0;
    let timeline = Timeline { clips: vec![first, next], tracks: vec![], captions: vec![] };
    let wires = [Wire::video("source")];
    let sources = [Source { name: "Original".into(), file: "media/original.mp4".into(), kind: MediaKind::Video, sound: false, duration_us: Some(5_000_000) }];
    let out = fcpxml(&Sequence {
        name: "Handles",
        width: 320,
        height: 180,
        fps: 10,
        background: [0; 3],
        timeline: &timeline,
        wires: &wires,
        media: &[Some(0)],
        sources: &sources,
    })
    .unwrap();
    assert!(out.xml.contains("start=\"15/10s\" duration=\"25/10s\""));
    assert_eq!(out.xml.matches("time=\"10/10s\" value=\"0.000000\"").count(), 2, "the second envelope keeps its incoming dissolve handle");
    assert_eq!(out.xml.matches("time=\"40/10s\" value=\"0.000000\"").count(), 2, "the first envelope keeps its outgoing dissolve handle");
}

#[test]
fn hostile_rates_and_aggregate_keyframes_are_bounded_and_cancellable() {
    let mut long = clip(Input::Audio, 1);
    long.out_us = Some(3_000_000);
    long.fade_in_us = 1_000_000;
    assert!(export(long.clone(), MediaKind::Audio, u32::MAX).unwrap_err().message.contains("too many fade keyframes"));
    let (env, _) = Envelope::new(&long, MediaKind::Audio, true, 10, 10, 20).unwrap();
    let mut check = || Ok(());
    let mut budget = Budget::new(&mut check);
    budget.remaining = 2;
    assert!(env.write(&mut String::new(), 10, 20, 10, "", &mut budget).is_err());
    let timeline = Timeline::default();
    let seq = Sequence { name: "Cancel", width: 1, height: 1, fps: 30, background: [0; 3], timeline: &timeline, wires: &[], media: &[], sources: &[] };
    let error = fcpxml_controlled(&seq, &mut || Err(Error::new(nodecraft_types::ErrorKind::Conflict, "cancelled test"))).unwrap_err();
    assert_eq!(error.message, "cancelled test");
}
