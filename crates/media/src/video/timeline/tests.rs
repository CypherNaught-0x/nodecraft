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
    Span {
        in_us,
        out_us,
        fade_in_us: 0,
        fade_out_us: 0,
        volume: 1000,
        muted: false,
        transition_us: 0,
        speed: 1000,
        placement: Placement::IDENTITY,
        look: Look::NONE,
    }
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
    let g = graph(FRAME, &[(Media::Video(&meta(64, 48, 0, true)), &span(0, 1_001_000))], &[], &[]).unwrap();
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
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &[], &[]).unwrap();
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
    let (vm, vs) = (Media::Video(&m), Media::Video(&sound));
    let g = graph(FRAME, &[(vm, &a), (vs, &b), (vm, &c), (vs, &d)], &[], &[]).unwrap();
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
#[test]
fn without_layers_the_graph_is_unchanged_and_with_them_tracks_stack_and_mix() {
    let m = meta(1280, 720, 0, true);
    let s = span(0, 2_000_000);
    let plain = graph(FRAME, &[(Media::Video(&m), &s)], &[], &[]).unwrap();
    assert!(!plain.contains("overlay") && !plain.contains("amix") && plain.contains("[vout]"), "{plain}");
    let music = AudioMetadata { duration_us: 9_000_000, stream: 0, codec: "flac", format_start: 0, stream_start: 0, sample_rate: 48_000, channels: 2 };
    let mut tune = span(1_000_000, 4_000_000);
    tune.fade_out_us = 500_000;
    let mut logo = span(0, 1_000_000);
    logo.fade_in_us = 200_000;
    let still = Media::Still { width: 64, height: 64 };
    let layers = [(Media::Audio(&music), &tune, 15u64), (still, &logo, 45u64), (Media::Video(&m), &s, 0u64)];
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &layers, &[]).unwrap();
    assert!(g.contains("[0:0]") && g.contains("[ma];"), "the main track ends in its own labels: {g}");
    // The tune runs 0.5 s to 3.5 s; the logo 1.5 s to 2.5 s; the main track only 2 s: padded
    // by the 1.5 s left, and a frame over.
    assert!(g.contains("[mv]tpad=stop_mode=add:stop_duration=1.533333:color=0x000000[b0];"), "{g}");
    assert!(!g.contains("[p2]"), "a layer's still keeps its alpha: {g}");
    assert!(g.contains("[2:0]setpts=PTS-STARTPTS,scale=720:720:flags=bicubic"), "the still is read raw: {g}");
    assert!(g.contains("format=yuva420p") && g.contains("color=black@0"), "a layer letterboxes to transparent: {g}");
    assert!(g.contains("fade=t=in:st=0:d=0.200000:color=0x000000:alpha=1,setpts=PTS+45[l1];"), "{g}");
    assert!(g.contains("[b0][l1]overlay=eof_action=pass:repeatlast=0:format=yuv420[b1];[3:0]"), "{g}");
    assert!(g.contains("[b1][l2]overlay"), "the video layer goes on top: {g}");
    assert!(g.contains("[b2]trim=end_frame=105,setpts=PTS-STARTPTS[vout]"), "{g}");
    assert!(g.contains("[1:0]aresample=48000") && g.contains(",adelay=delays=24000S:all=1[s0];"), "half a second at 48 kHz: {g}");
    assert!(g.contains("[3:1]aresample") && g.contains("[s2];"), "a video layer's own sound joins the mix: {g}");
    assert!(g.ends_with("[ma][s0][s2]amix=inputs=3:duration=longest:dropout_transition=0:normalize=0,apad,atrim=duration=3.500000[aout]"), "{g}");
}
#[test]
fn layers_stay_bounded() {
    let card = Frame { width: 64, height: 48, fps: (30, 1), background: [0, 0, 0] };
    assert_eq!(frames_at(1_016_000, card.fps), 30, "positions round to the nearest frame");
    assert_eq!(frame_us(45, (30, 1)), 1_500_000);
    assert_eq!(
        graph(
            card,
            &[(
                Media::Audio(&AudioMetadata { duration_us: 1, stream: 0, codec: "mp3", format_start: 0, stream_start: 0, sample_rate: 48_000, channels: 2 }),
                &span(0, 1)
            )],
            &[],
            &[]
        ),
        Err(VideoError::Invalid)
    );
}

#[test]
fn a_still_on_the_main_track_shows_the_background_through_its_transparent_pixels() {
    let still = Media::Still { width: 64, height: 32 };
    let s = span(0, 1_000_000);
    let g = graph(FRAME, &[(still, &s)], &[], &[]).unwrap();
    assert!(g.contains("format=rgba[p0];color=c=0x000000:s=1280x640:r=30/1:d=1.000000[q0];"), "{g}");
    assert!(g.contains("[q0][p0]overlay,format=yuv420p,setparams=range=tv:color_primaries=bt709"), "{g}");
}

#[test]
fn captions_go_over_everything_at_their_frames() {
    let m = meta(1280, 720, 0, true);
    let s = span(0, 2_000_000);
    let captions = [Overlay { x: 100, y: 600, from: 15, to: 45 }, Overlay { x: 0, y: 0, from: 0, to: 60 }];
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &[], &captions).unwrap();
    assert!(g.contains("[cv0];") && !g.contains("[vout];[1:0]"), "the picture goes into the first caption's base: {g}");
    // Frames 15 to 44 start at 0.5 s to 1.4667 s; half a frame either side bounds them.
    assert!(
        g.contains("[1:0]scale=out_color_matrix=bt709:out_range=tv,format=yuva420p,setpts=PTS-STARTPTS[t0];[cv0][t0]overlay=x=100:y=600:eof_action=repeat:format=yuv420:enable='between(t,0.483334,1.483334)'[cv1];"),
        "{g}"
    );
    assert!(
        g.contains("[2:0]scale") && g.contains("[cv1][t1]overlay=x=0:y=0:eof_action=repeat:format=yuv420:enable='between(t,0.000000,1.983334)'[vout]"),
        "{g}"
    );
    assert!(g.contains("[aout]") && !g.ends_with(';'), "{g}");
    // With layers the captions come after their inputs and over the stacked picture.
    let still = Media::Still { width: 64, height: 64 };
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &[(still, &span(0, 1_000_000), 0)], &captions[..1]).unwrap();
    assert!(g.contains("setpts=PTS-STARTPTS[cv0];") && g.contains("[2:0]scale=out_color_matrix") && g.contains("[cv0][t0]overlay=x=100:y=600"), "{g}");
    assert!(g.contains("[vout]") && !g.contains("[vout];[2:0]"), "{g}");
}

#[test]
fn speed_shortens_the_output_and_tempo_stays_within_atempo_range() {
    let m = meta(1280, 720, 0, true);
    let s = Span { speed: 2000, ..span(0, 4_000_000) };
    assert_eq!((s.length_us(), s.media_us()), (2_000_000, 4_000_000));
    assert_eq!(Span { speed: 3000, ..span(0, 1) }.length_us(), 1, "a played clip lasts at least a microsecond");
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &[], &[]).unwrap();
    assert!(g.contains("[0:0]setpts=(PTS-STARTPTS)*1000/2000,colorspace"), "{g}");
    assert!(g.contains("asetpts=PTS-STARTPTS,atempo=2.000,apad,atrim=duration=2.000000"), "{g}");
    assert!(g.contains("trim=end_frame=60"), "{g}");
    assert_eq!(tempo(1000), "");
    assert_eq!(tempo(250), ",atempo=0.5,atempo=0.500");
    assert_eq!(tempo(750), ",atempo=0.750");
    assert_eq!(tempo(3001), ",atempo=2.0,atempo=1.5005");
    assert_eq!(tempo(4000), ",atempo=2.0,atempo=2.0000");
}

#[test]
fn a_placed_picture_is_cropped_sized_turned_and_drawn_where_it_goes() {
    let m = meta(1280, 720, 0, false);
    let placement = Placement { pos_x: 250, pos_y: 750, scale: 500, crop: [100, 0, 100, 0], rotation: 90, opacity: 500 };
    let s = Span { placement, ..span(0, 1_000_000) };
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &[], &[]).unwrap();
    // 80% of the width, fitted to 1280x720 at 1024x720, then halved.
    assert!(g.contains("crop=w=max(1\\,iw*800/1000):h=max(1\\,ih*1000/1000):x=iw*100/1000:y=ih*0/1000,scale=512:360:"), "{g}");
    assert!(g.contains("format=yuva444p,rotate=a=90*PI/180:ow=360:oh=512:c=black@0,lutyuv=a=val*500/1000,setsar=1[p0];"), "{g}");
    // Centre at (320, 540) less half the turned box (180, 256).
    assert!(g.contains("color=c=0x000000:s=1280x720:r=30/1:d=1.000000,format=yuv420p[q0];[q0][p0]overlay=x=140:y=284:eof_action=repeat:format=yuv420,format=yuv420p,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709,setsar=1,fps="), "{g}");
    assert!(!g.contains(",pad="), "{g}");
    // Off the top-left corner, on a layer: transparent around it.
    let off = Placement { pos_x: -100, pos_y: 0, ..Placement::IDENTITY };
    let still = Media::Still { width: 64, height: 64 };
    let l = Span { placement: off, ..span(0, 1_000_000) };
    let g = graph(FRAME, &[(Media::Video(&m), &span(0, 1_000_000))], &[(still, &l, 0)], &[]).unwrap();
    assert!(g.contains("color=c=black@0:s=1280x720:r=30/1:d=1.000000,format=yuva420p[q1];[q1][p1]overlay=x=-488:y=-360:"), "{g}");
    // A still on the main track needs no box of its own: the frame behind it shows through.
    let g = graph(FRAME, &[(still, &l)], &[], &[]).unwrap();
    assert!(!g.contains("format=rgba[p0]") && g.contains("[q0][p0]overlay=x=-488"), "{g}");
}

#[test]
fn placements_stay_bounded() {
    assert_eq!(turned(100, 50, 0), (100, 50));
    assert_eq!(turned(100, 50, 90), (50, 100));
    assert_eq!(turned(100, 50, -180), (100, 50));
    assert_eq!(turned(100, 100, 45), (142, 142));
    let big = Placement { scale: 4000, ..Placement::IDENTITY };
    let frame = Frame { width: 4096, height: 4096, fps: (30, 1), background: [0, 0, 0] };
    assert_eq!(placed_size((1, 1), big, frame), Err(VideoError::Limit));
    let tiny = Placement { scale: 10, ..Placement::IDENTITY };
    assert_eq!(placed_size((1280, 720), tiny, FRAME), Ok((12, 6)));
    let tinier = Placement { scale: 10, crop: [0, 0, 0, 0], ..Placement::IDENTITY };
    assert_eq!(placed_size((16, 16), tinier, Frame { width: 16, height: 16, ..FRAME }), Ok((2, 2)));
    assert!(!Placement { crop: [450, 0, 450, 0], ..Placement::IDENTITY }.valid());
    assert!(!Placement { rotation: 181, ..Placement::IDENTITY }.valid());
    assert!(!Placement { opacity: 1001, ..Placement::IDENTITY }.valid());
    assert!(!Placement { pos_x: 2001, ..Placement::IDENTITY }.valid());
    assert!(Placement { pos_x: -1000, pos_y: 2000, crop: [449, 0, 450, 899], ..Placement::IDENTITY }.valid());
}

#[test]
fn qualities_pick_their_own_vp9_settings() {
    assert_eq!(Quality::default(), Quality::Standard);
    assert_eq!(Quality::Standard.vp9(), ("32", "4000000"), "standard is the encoding used before qualities");
    assert_ne!(Quality::Draft.vp9(), Quality::High.vp9());
}

#[test]
fn a_layer_fits_in_the_frames_the_main_track_shows() {
    // A 10 s video at 30 a second with a sound from 0.25 s: the start rounds to frame 8, so the
    // sound gets 292 frames and ends with the video at frame 300, not a frame after it.
    let room = layer_room_us(10_000_000, 250_000, (30, 1)).unwrap();
    assert_eq!((frames_at(250_000, (30, 1)), frames(room, (30, 1))), (8, 292));
    assert_eq!(layer_room_us(10_000_000, 20_000, (25, 1)).map(|us| frames(us, (25, 1))), Some(249));
    assert_eq!(layer_room_us(10_000_000, 0, (30000, 1001)).map(|us| frames(us, (30000, 1001))), Some(frames(10_000_000, (30000, 1001))));
    // Starting at or past the end leaves no room.
    assert_eq!(layer_room_us(1_000_000, 1_000_000, (30, 1)), None);
    assert_eq!(layer_room_us(1_000_000, 990_000, (30, 1)), None);
    assert_eq!(layer_room_us(0, 0, (30, 1)).map(|us| frames(us, (30, 1))), Some(1));
    assert_eq!(layer_room_us(u64::MAX, u64::MAX, (120_000, 1)), None);
}
#[test]
fn a_look_grades_adds_its_effect_and_keys_before_the_picture_is_scaled() {
    let m = meta(1280, 720, 0, false);
    let plain = graph(FRAME, &[(Media::Video(&m), &span(0, 1_000_000))], &[], &[]).unwrap();
    assert!(!plain.contains("eq=") && !plain.contains("chromakey"), "no look, no filters: {plain}");
    let graded = Look { brightness: -250, contrast: 1500, saturation: 0, gamma: 1200, effect: Effect::Sepia, key: None };
    let s = Span { look: graded, ..span(0, 1_000_000) };
    let g = graph(FRAME, &[(Media::Video(&m), &s)], &[], &[]).unwrap();
    assert!(
        g.contains("dither=none,lutrgb=r=val-64:g=val-64:b=val-64,eq=contrast=1.500:saturation=0.000:gamma=1.200,hue=s=0,lutyuv=u=108:v=146,scale="),
        "{g}"
    );
    for (effect, filter) in [
        (Effect::Grayscale, "hue=s=0,"),
        (Effect::Invert, "negate,"),
        (Effect::Blur(2500), "gblur=sigma=2.500,"),
        (Effect::Sharpen(1000), "unsharp=lx=5:ly=5:la=1.000:cx=5:cy=5:ca=0,"),
        (Effect::Vignette(500), "vignette=angle=PI*0.500/2,"),
    ] {
        let s = Span { look: Look { effect, ..Look::NONE }, ..span(0, 1_000_000) };
        let g = graph(FRAME, &[(Media::Video(&m), &s)], &[], &[]).unwrap();
        assert!(g.contains(&format!("dither=none,{filter}scale=")), "{effect:?}: {g}");
    }
    let keyed = Look { key: Some(Key { color: [0, 255, 0], similarity: 150, blend: 50 }), ..Look::NONE };
    let layer = Span { look: keyed, ..span(0, 1_000_000) };
    let g = graph(FRAME, &[(Media::Video(&m), &span(0, 1_000_000))], &[(Media::Video(&m), &layer, 0)], &[]).unwrap();
    assert!(g.contains("format=yuva444p,chromakey=color=0x00ff00:similarity=0.150:blend=0.050,scale="), "{g}");
    assert!(g.contains("format=yuva420p"), "the keyed layer keeps its alpha: {g}");
}
#[test]
fn looks_stay_bounded_and_keys_only_go_on_layers() {
    for look in [
        Look { brightness: 1001, ..Look::NONE },
        Look { contrast: 3001, ..Look::NONE },
        Look { saturation: 3001, ..Look::NONE },
        Look { gamma: 99, ..Look::NONE },
        Look { effect: Effect::Blur(0), ..Look::NONE },
        Look { effect: Effect::Blur(MAX_BLUR + 1), ..Look::NONE },
        Look { effect: Effect::Sharpen(3001), ..Look::NONE },
        Look { effect: Effect::Vignette(1001), ..Look::NONE },
        Look { key: Some(Key { color: [0; 3], similarity: 9, blend: 0 }), ..Look::NONE },
        Look { key: Some(Key { color: [0; 3], similarity: 100, blend: 1001 }), ..Look::NONE },
    ] {
        assert!(!look.valid(), "{look:?}");
    }
    assert!(Look::NONE.valid());
    assert_eq!(look(&Look::NONE), "");
    assert_eq!(look(&Look { brightness: 1000, ..Look::NONE }), "lutrgb=r=val+255:g=val+255:b=val+255,");
}
