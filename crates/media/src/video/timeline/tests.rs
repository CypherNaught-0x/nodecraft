use super::*;
fn meta(width: u32, height: u32, rotation: u16, audio: bool) -> VideoMetadata {
    VideoMetadata {
        width,
        height,
        duration_us: 10_000_000,
        stream: 0,
        rotation,
        sar: (1, 1),
        frame_rate: Some((30, 1)),
        color: VideoColorInfo { original: [None, None, None, None], profile: ColorProfile::Bt709, full_range: false, assumed_fields: 15 },
        format_start: 0,
        stream_start: 40_000,
        timebase: (1, 1000),
        frames: None,
        audio: audio.then_some((1, "aac")),
    }
}
fn span(in_us: u64, out_us: u64) -> Span {
    Span { in_us, out_us, fade_in_us: 0, fade_out_us: 0, volume: 1000, muted: false, transition_us: 0 }
}
const FRAME: Frame = Frame { width: 1280, height: 720, fps: (30, 1), background: [0, 0, 0] };

#[test]
fn spans_must_fit_their_clips_and_transitions() {
    assert!(validate(FRAME, &[span(0, 1_000_000)]).is_ok());
    assert_eq!(validate(FRAME, &[]), Err(VideoError::Invalid));
    assert_eq!(validate(FRAME, &[span(5, 5)]), Err(VideoError::Invalid));
    let mut last = span(0, 1_000_000);
    last.transition_us = 100_000;
    assert_eq!(validate(FRAME, &[last]), Err(VideoError::Invalid), "the last clip has nothing to cross into");
    let mut a = span(0, 1_000_000);
    a.transition_us = 600_000;
    let mut b = span(0, 1_000_000);
    b.transition_us = 600_000;
    assert_eq!(validate(FRAME, &[a, b, span(0, 1_000_000)]), Err(VideoError::Invalid), "two transitions overlap inside b");
    b.transition_us = 300_000;
    assert!(validate(FRAME, &[a, b, span(0, 1_000_000)]).is_ok());
    let mut tiny = span(0, 1_000_000);
    tiny.transition_us = 1;
    assert_eq!(validate(FRAME, &[tiny, span(0, 1_000_000)]), Err(VideoError::Invalid));
    let mut loud = span(0, 1_000_000);
    loud.volume = MAX_VOLUME + 1;
    assert_eq!(validate(FRAME, &[loud]), Err(VideoError::Invalid));
    let mut faded = span(0, 1_000_000);
    (faded.fade_in_us, faded.fade_out_us) = (600_000, 500_000);
    assert_eq!(validate(FRAME, &[faded]), Err(VideoError::Invalid));
    assert_eq!(validate(Frame { width: 1281, ..FRAME }, &[span(0, 1_000_000)]), Err(VideoError::Invalid));
    let hour = span(0, 3_000_000_000);
    assert_eq!(validate(FRAME, &[hour, hour]), Err(VideoError::Limit));
    assert_eq!(validate(FRAME, &vec![span(0, 1_000_000); MAX_CLIPS + 1]), Err(VideoError::Invalid));
}
#[test]
fn duration_subtracts_each_overlap() {
    let mut a = span(0, 2_000_000);
    a.transition_us = 500_000;
    assert_eq!(duration([a, span(1_000_000, 4_000_000)].iter()), 4_500_000);
}
#[test]
fn clips_show_whole_frames() {
    assert_eq!((frames(1_001_000, (30, 1)), shown_us(1_001_000, (30, 1))), (30, 1_000_000));
    assert_eq!((frames(1_020_000, (30, 1)), shown_us(1_020_000, (30, 1))), (31, 1_033_333));
    assert_eq!((frames(2_041_667, (30000, 1001)), shown_us(2_041_667, (30000, 1001))), (61, 2_035_367));
    assert_eq!(frames(1, (30, 1)), 1, "never no frames");
    let g = graph(FRAME, &[(&meta(64, 48, 0, true), &span(0, 1_001_000))]).unwrap();
    assert!(g.contains("trim=end_frame=30,") && g.contains("atrim=duration=1.000000,"), "{g}");
}
#[test]
fn letterboxing_keeps_display_aspect_and_even_sizes() {
    assert_eq!(fit(&meta(640, 480, 0, false), 1280, 720), (960, 720));
    assert_eq!(fit(&meta(1920, 1080, 90, false), 1280, 720), (404, 720));
    let mut anamorphic = meta(720, 576, 0, false);
    anamorphic.sar = (16, 11);
    assert_eq!(fit(&anamorphic, 1280, 720), (1280, 704));
    assert_eq!(fit(&meta(2, 4096, 0, false), 1280, 720), (2, 720));
}
#[test]
fn seek_starts_at_the_video_stream() {
    assert_eq!(seek(&meta(16, 16, 0, false), &span(1_500_000, 2_000_000)), "1.540000");
    assert_eq!(seconds(3_000_007), "3.000007");
}
#[test]
fn one_clip_maps_straight_to_the_outputs() {
    let m = meta(1280, 720, 0, false);
    let s = span(0, 1_000_000);
    let g = graph(FRAME, &[(&m, &s)]).unwrap();
    assert!(g.contains("[0:0]setpts=PTS-STARTPTS,colorspace="), "{g}");
    assert!(g.ends_with("[aout]"), "{g}");
    assert!(g.contains("[vout];anullsrc=r=48000"), "silence stands in for a clip with no audio: {g}");
    assert!(!g.contains("concat") && !g.contains("xfade"));
}
#[test]
fn cuts_concat_and_transitions_cross_at_the_running_length() {
    let (m, sound) = (meta(1280, 720, 0, false), meta(1280, 720, 90, true));
    let mut a = span(0, 2_000_000);
    (a.fade_in_us, a.transition_us) = (250_000, 0);
    let mut b = span(1_000_000, 4_000_000);
    (b.transition_us, b.volume, b.fade_in_us) = (500_000, 1250, 250_000);
    let mut c = span(0, 2_000_000);
    (c.fade_out_us, c.transition_us) = (400_000, 1_000_000);
    let mut d = span(0, 3_000_000);
    d.muted = true;
    let g = graph(FRAME, &[(&m, &a), (&sound, &b), (&m, &c), (&sound, &d)]).unwrap();
    assert!(g.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[c1][y1];[c1]settb=1/30[x1]"), "{g}");
    // a and b run 5 s; the cross into c starts half a second before that.
    assert!(g.contains("[x1][v2]xfade=transition=fade:duration=0.500000:offset=4.500000[x2]"), "{g}");
    assert!(g.contains("[y1][a2]acrossfade=d=0.500000:c1=tri:c2=tri[y2]"), "{g}");
    // 6.5 s so far; c's one-second cross into d starts at 5.5 s.
    assert!(g.contains("[x2][v3]xfade=transition=fade:duration=1.000000:offset=5.500000[vout]"), "{g}");
    assert!(g.contains("[1:1]aresample=48000"), "b's own audio stream: {g}");
    assert!(g.contains("volume=1.250"), "{g}");
    assert!(!g.contains("[3:1]"), "muted d is silence: {g}");
    assert!(g.contains("transpose=clock,scale=404:720"), "{g}");
    assert!(g.contains("fade=t=out:st=1.600000:d=0.400000:color=0x000000"), "{g}");
    assert!(g.contains("volume=1.250,afade=t=in:st=0:d=0.250000[a1]"), "{g}");
    assert!(!g.ends_with(';'));
}
