//! A typed timeline of staged clips, and the FFmpeg filter graph that renders it.
//! Every number on the command line is formatted here from validated integers; no
//! caller string ever reaches FFmpeg.
use super::*;
use std::fmt::Write as _;

/// Most clips one composition may stitch together.
pub const MAX_CLIPS: usize = 32;
/// Longest composition, in microseconds (one hour).
pub const MAX_DURATION_US: u64 = 3_600_000_000;
/// Loudest clip volume, in thousandths (4.0×).
pub const MAX_VOLUME: u16 = 4000;
/// Shortest cross transition, in microseconds; anything shorter is a cut.
pub const MIN_TRANSITION_US: u64 = 10_000;

/// When and how loudly one clip plays, in the clip's own microseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) in_us: u64,
    pub(crate) out_us: u64,
    pub(crate) fade_in_us: u64,
    pub(crate) fade_out_us: u64,
    pub(crate) volume: u16,
    pub(crate) muted: bool,
    pub(crate) transition_us: u64,
}
impl Span {
    fn length_us(&self) -> u64 {
        self.out_us.saturating_sub(self.in_us)
    }
}

/// What a clip plays: a staged video, a sound-only file or a still picture.
#[derive(Clone)]
pub enum Source {
    Video(VideoAsset),
    Audio(AudioAsset),
    Still(StillAsset),
}
/// A source as the graph sees it.
#[derive(Clone, Copy)]
pub(crate) enum Media<'a> {
    Video(&'a VideoMetadata),
    Audio(&'a AudioMetadata),
    Still { width: u32, height: u32 },
}
impl Source {
    pub(crate) fn media(&self) -> Media<'_> {
        match self {
            Self::Video(a) => Media::Video(a.metadata()),
            Self::Audio(a) => Media::Audio(a.metadata()),
            Self::Still(s) => Media::Still { width: s.width(), height: s.height() },
        }
    }
}
impl Media<'_> {
    fn visual(&self) -> bool {
        !matches!(self, Self::Audio(_))
    }
}

/// One span of a staged clip on the timeline.
#[derive(Clone)]
pub struct ComposeClip {
    pub(crate) source: Source,
    pub(crate) span: Span,
}
impl ComposeClip {
    /// The span `[in_us, out_us)` of `asset`, at full volume, with no fades.
    pub fn new(asset: VideoAsset, in_us: u64, out_us: u64) -> Result<Self> {
        let duration = asset.metadata().duration_us;
        Self::spanning(Source::Video(asset), in_us, out_us, duration)
    }
    /// The span `[in_us, out_us)` of a sound-only file.
    pub fn audio(asset: AudioAsset, in_us: u64, out_us: u64) -> Result<Self> {
        let duration = asset.metadata().duration_us;
        Self::spanning(Source::Audio(asset), in_us, out_us, duration)
    }
    /// A still picture held for `length_us`.
    pub fn still(asset: StillAsset, length_us: u64) -> Result<Self> {
        Self::spanning(Source::Still(asset), 0, length_us, MAX_DURATION_US)
    }
    fn spanning(source: Source, in_us: u64, out_us: u64, duration: u64) -> Result<Self> {
        if in_us >= out_us || out_us > duration {
            return Err(VideoError::Invalid);
        }
        let span = Span { in_us, out_us, fade_in_us: 0, fade_out_us: 0, volume: 1000, muted: false, transition_us: 0 };
        Ok(Self { source, span })
    }
    pub fn source(&self) -> &Source {
        &self.source
    }
    pub fn length_us(&self) -> u64 {
        self.span.length_us()
    }
    /// Fade from and to the background colour (and silence) over the clip's own ends.
    /// On a layer the picture fades to transparent instead.
    pub fn with_fades(mut self, fade_in_us: u64, fade_out_us: u64) -> Result<Self> {
        if fade_in_us.checked_add(fade_out_us).is_none_or(|sum| sum > self.length_us()) {
            return Err(VideoError::Invalid);
        }
        self.span.fade_in_us = fade_in_us;
        self.span.fade_out_us = fade_out_us;
        Ok(self)
    }
    /// Volume in thousandths (1000 is unchanged); `muted` replaces the clip's audio with silence.
    pub fn with_volume(mut self, volume: u16, muted: bool) -> Result<Self> {
        if volume > MAX_VOLUME {
            return Err(VideoError::Invalid);
        }
        self.span.volume = volume;
        self.span.muted = muted;
        Ok(self)
    }
    /// Cross-dissolve picture and sound into the next clip over this many microseconds.
    pub fn with_transition(mut self, transition_us: u64) -> Result<Self> {
        if (transition_us != 0 && transition_us < MIN_TRANSITION_US) || transition_us > self.length_us() {
            return Err(VideoError::Invalid);
        }
        self.span.transition_us = transition_us;
        Ok(self)
    }
}

/// A clip placed on a layer above the main track, starting `at` whole output frames in.
#[derive(Clone)]
pub(crate) struct Layer {
    pub(crate) clip: ComposeClip,
    pub(crate) at: u64,
}

/// The output frame, the main track of pictures played one after another, and the layers over it.
#[derive(Clone)]
pub struct ComposeSpec {
    pub(crate) frame: Frame,
    pub(crate) clips: Vec<ComposeClip>,
    pub(crate) layers: Vec<Layer>,
}
/// The output picture: size, rate and the colour behind letterboxing and fades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: (u32, u32),
    pub(crate) background: [u8; 3],
}
impl ComposeSpec {
    /// The main track: video and still clips, played in order. Sound-only clips go on layers.
    pub fn new(width: u32, height: u32, fps: (u32, u32), background: [u8; 3], clips: Vec<ComposeClip>) -> Result<Self> {
        let frame = Frame { width, height, fps, background };
        if clips.iter().any(|c| !c.source.media().visual()) {
            return Err(VideoError::Invalid);
        }
        let spans: Vec<Span> = clips.iter().map(|c| c.span).collect();
        validate(frame, &spans)?;
        Ok(Self { frame, clips, layers: Vec::new() })
    }
    /// Add clips that start at their own time, each `(clip, at_us)`, rounded to whole frames.
    /// Pictures stack in order over the main track (the last on top); every sound is mixed in.
    /// Layers take no transitions, and the output runs until the last track ends.
    pub fn with_layers(mut self, layers: Vec<(ComposeClip, u64)>) -> Result<Self> {
        if self.clips.len().saturating_add(layers.len()) > MAX_CLIPS {
            return Err(VideoError::Invalid);
        }
        for (clip, at_us) in layers {
            let span = &clip.span;
            if span.in_us >= span.out_us
                || span.volume > MAX_VOLUME
                || span.fade_in_us.saturating_add(span.fade_out_us) > span.length_us()
                || span.transition_us != 0
            {
                return Err(VideoError::Invalid);
            }
            if at_us > MAX_DURATION_US {
                return Err(VideoError::Limit);
            }
            let at = frames_at(at_us, self.frame.fps);
            if frame_us(at, self.frame.fps).saturating_add(shown_us(span.length_us(), self.frame.fps)) > MAX_DURATION_US {
                return Err(VideoError::Limit);
            }
            self.layers.push(Layer { clip, at });
        }
        Ok(self)
    }
    /// Running time of the output: the main track (every clip as whole frames, less the overlap
    /// of each transition) or the last layer's end, whichever is later.
    pub fn duration_us(&self) -> u64 {
        let total = self.clips.iter().map(|c| shown_us(c.span.length_us(), self.frame.fps)).fold(0u64, u64::saturating_add);
        let main = total.saturating_sub(self.clips.iter().map(|c| c.span.transition_us).fold(0u64, u64::saturating_add));
        self.layers.iter().map(|l| layer_end_us(l, self.frame.fps)).fold(main, u64::max)
    }
    pub fn clips(&self) -> &[ComposeClip] {
        &self.clips
    }
    /// Every input in command-line order: the main track, then the layers.
    pub(crate) fn inputs(&self) -> impl Iterator<Item = &ComposeClip> {
        self.clips.iter().chain(self.layers.iter().map(|l| &l.clip))
    }
    pub(super) fn parts(&self) -> Vec<(Media<'_>, &Span)> {
        self.clips.iter().map(|c| (c.source.media(), &c.span)).collect()
    }
    pub(super) fn layer_parts(&self) -> Vec<(Media<'_>, &Span, u64)> {
        self.layers.iter().map(|l| (l.clip.source.media(), &l.clip.span, l.at)).collect()
    }
}
fn layer_end_us(l: &Layer, fps: (u32, u32)) -> u64 {
    frame_us(l.at, fps).saturating_add(shown_us(l.clip.span.length_us(), fps))
}
fn duration<'a>(spans: impl Iterator<Item = &'a Span> + Clone) -> u64 {
    let total = spans.clone().map(Span::length_us).fold(0u64, u64::saturating_add);
    let overlap = spans.map(|s| s.transition_us).fold(0u64, u64::saturating_add);
    total.saturating_sub(overlap)
}
pub(crate) fn validate(frame: Frame, spans: &[Span]) -> Result<()> {
    // The frame and rate rules are the renderer's; one frame stands in for the count.
    RenderSpec::new(frame.width, frame.height, frame.fps, 1, frame.background)?;
    if spans.is_empty() || spans.len() > MAX_CLIPS {
        return Err(VideoError::Invalid);
    }
    let period = u64::from(frame.fps.1).saturating_mul(1_000_000).div_ceil(u64::from(frame.fps.0));
    let mut before = 0u64;
    for (index, span) in spans.iter().enumerate() {
        let last = index + 1 == spans.len();
        if span.in_us >= span.out_us
            || span.volume > MAX_VOLUME
            || span.fade_in_us.saturating_add(span.fade_out_us) > span.length_us()
            || (last && span.transition_us != 0)
            || (span.transition_us != 0 && span.transition_us < MIN_TRANSITION_US)
        {
            return Err(VideoError::Invalid);
        }
        // Each clip shows for at least a frame outside the transitions it takes part in.
        if before.saturating_add(span.transition_us).saturating_add(period) > span.length_us() {
            return Err(VideoError::Invalid);
        }
        before = span.transition_us;
    }
    if duration(spans.iter()) > MAX_DURATION_US {
        return Err(VideoError::Limit);
    }
    Ok(())
}

/// How many output frames a clip of `us` shows: its length rounded to whole frames, at least one.
/// Every clip is cut to whole frames, so cuts add no stray frame each (the trim would keep one).
fn frames(us: u64, fps: (u32, u32)) -> u64 {
    let unit = u128::from(fps.1).saturating_mul(1_000_000).max(1);
    let n = (u128::from(us).saturating_mul(u128::from(fps.0)).saturating_add(unit / 2)) / unit;
    u64::try_from(n).unwrap_or(u64::MAX).max(1)
}
/// The microseconds `frames(us)` whole frames last, to the nearest microsecond.
pub(crate) fn shown_us(us: u64, fps: (u32, u32)) -> u64 {
    let rate = u128::from(fps.0).max(1);
    let exact = u128::from(frames(us, fps)).saturating_mul(u128::from(fps.1)).saturating_mul(1_000_000);
    u64::try_from(exact.saturating_add(rate / 2) / rate).unwrap_or(u64::MAX)
}

/// The whole frame nearest `us`, counting from 0.
fn frames_at(us: u64, fps: (u32, u32)) -> u64 {
    let unit = u128::from(fps.1).saturating_mul(1_000_000).max(1);
    let n = (u128::from(us).saturating_mul(u128::from(fps.0)).saturating_add(unit / 2)) / unit;
    u64::try_from(n).unwrap_or(u64::MAX)
}
/// When frame `n` starts, to the nearest microsecond.
fn frame_us(n: u64, fps: (u32, u32)) -> u64 {
    let rate = u128::from(fps.0).max(1);
    let exact = u128::from(n).saturating_mul(u128::from(fps.1)).saturating_mul(1_000_000);
    u64::try_from(exact.saturating_add(rate / 2) / rate).unwrap_or(u64::MAX)
}

/// Seconds with microsecond precision, as FFmpeg reads durations.
pub(super) fn seconds(us: u64) -> String {
    format!("{}.{:06}", us / 1_000_000, us % 1_000_000)
}
/// Where the clip's video stream starts reading, in seconds from the file's start.
pub(super) fn seek(m: &VideoMetadata, span: &Span) -> String {
    start(m.format_start, m.stream_start, span.in_us)
}
/// Where a sound-only clip's stream starts reading, in seconds from the file's start.
pub(super) fn seek_audio(m: &AudioMetadata, span: &Span) -> String {
    start(m.format_start, m.stream_start, span.in_us)
}
fn start(format_start: i128, stream_start: i128, in_us: u64) -> String {
    let offset = u64::try_from(stream_start.saturating_sub(format_start)).unwrap_or(0);
    seconds(offset.saturating_add(in_us))
}
/// The largest even size with the clip's display aspect that fits `width`×`height`.
fn fit(m: &VideoMetadata, width: u32, height: u32) -> (u32, u32) {
    let mut w = u64::from(m.width) * u64::from(m.sar.0.max(1));
    let mut h = u64::from(m.height) * u64::from(m.sar.1.max(1));
    if matches!(m.rotation, 90 | 270) {
        std::mem::swap(&mut w, &mut h);
    }
    fit_display(w, h, width, height)
}
fn fit_display(w: u64, h: u64, width: u32, height: u32) -> (u32, u32) {
    let (bw, bh) = (u64::from(width), u64::from(height));
    let (fw, fh) = if w.saturating_mul(bh) > h.saturating_mul(bw) { (bw, h.saturating_mul(bw) / w.max(1)) } else { (w.saturating_mul(bh) / h.max(1), bh) };
    let even = |v: u64, cap: u32| u32::try_from(v & !1).unwrap_or(cap).clamp(2, cap);
    (even(fw, width), even(fh, height))
}
fn hex(rgb: [u8; 3]) -> String {
    format!("0x{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// Input `k`'s picture, fitted into the frame and cut to whole frames, labelled `[out]`.
/// A layer's picture keeps its alpha, letterboxes and fades to transparent, and starts `at` frames in.
fn picture(g: &mut String, k: usize, media: Media<'_>, span: &Span, frame: Frame, layer: Option<u64>, out: &str) -> Result<()> {
    let color = hex(frame.background);
    let rate = format!("{}/{}", frame.fps.0, frame.fps.1);
    let shown = shown_us(span.length_us(), frame.fps);
    let length = seconds(shown);
    let count = frames(span.length_us(), frame.fps);
    let (format, pad, alpha) = if layer.is_some() { ("yuva420p", "black@0", ":alpha=1") } else { ("yuv420p", color.as_str(), "") };
    let (w, h) = match media {
        Media::Video(m) => {
            let (p, t) = match m.color.profile {
                ColorProfile::Bt709 => ("bt709", "bt709"),
                ColorProfile::Smpte170m => ("smpte170m", "smpte170m"),
                ColorProfile::Bt470bg => ("bt470bg", "bt709"),
            };
            let t = match m.color.original.get(2).and_then(Option::as_deref) {
                Some("bt709") => "bt709",
                Some("smpte170m") => "smpte170m",
                _ => t,
            };
            let range = if m.color.full_range { "pc" } else { "tv" };
            let rotate = match m.rotation {
                90 => "transpose=clock,",
                180 => "hflip,vflip,",
                270 => "transpose=cclock,",
                _ => "",
            };
            let (w, h) = fit(m, frame.width, frame.height);
            write!(
                g,
                "[{k}:{stream}]setpts=PTS-STARTPTS,colorspace=ispace={p}:iprimaries={p}:itrc={t}:irange={range}:all=bt709:range=tv:format=yuv444p:fast=0:dither=none,\
                 {rotate}scale={w}:{h}:flags=bicubic:in_color_matrix=bt709:out_color_matrix=bt709:in_range=tv:out_range=tv,",
                stream = m.stream,
            )
            .map_err(|_| VideoError::Limit)?;
            (w, h)
        }
        // Raw RGBA staged by the service: no colour metadata to honour, sRGB into BT.709.
        Media::Still { width, height } => {
            let (w, h) = fit_display(u64::from(width), u64::from(height), frame.width, frame.height);
            write!(g, "[{k}:0]setpts=PTS-STARTPTS,scale={w}:{h}:flags=bicubic:out_color_matrix=bt709:out_range=tv,").map_err(|_| VideoError::Limit)?;
            if layer.is_none() {
                // On the main track a transparent pixel shows the background, not the colour it
                // hides: the picture goes over a plain frame as long as it is shown.
                write!(g, "format=rgba[p{k}];color=c={color}:s={w}x{h}:r={rate}:d={length}[q{k}];[q{k}][p{k}]overlay,").map_err(|_| VideoError::Limit)?;
            }
            (w, h)
        }
        Media::Audio(_) => return Err(VideoError::Invalid),
    };
    let x = (frame.width.saturating_sub(w) / 2) & !1;
    let y = (frame.height.saturating_sub(h) / 2) & !1;
    // A still has no colour of its own to pass on: tagged as the videos are, so a render of
    // stills alone still declares its colour.
    let tag = if matches!(media, Media::Still { .. }) { "setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709," } else { "" };
    write!(
        g,
        "format={format},{tag}setsar=1,pad={W}:{H}:{x}:{y}:color={pad},fps={rate},tpad=stop_mode=clone:stop_duration={length},trim=end_frame={count},setpts=PTS-STARTPTS",
        W = frame.width,
        H = frame.height,
    )
    .map_err(|_| VideoError::Limit)?;
    if span.fade_in_us > 0 {
        write!(g, ",fade=t=in:st=0:d={}:color={color}{alpha}", seconds(span.fade_in_us)).map_err(|_| VideoError::Limit)?;
    }
    if span.fade_out_us > 0 {
        write!(g, ",fade=t=out:st={}:d={}:color={color}{alpha}", seconds(shown.saturating_sub(span.fade_out_us)), seconds(span.fade_out_us))
            .map_err(|_| VideoError::Limit)?;
    }
    // After `fps` a timestamp counts frames.
    if let Some(at) = layer.filter(|at| *at > 0) {
        write!(g, ",setpts=PTS+{at}").map_err(|_| VideoError::Limit)?;
    }
    write!(g, "[{out}];").map_err(|_| VideoError::Limit)
}
/// Input `k`'s sound from `stream`, resampled, cut to the clip's frames and faded, unlabelled.
fn sound(g: &mut String, k: usize, stream: u32, span: &Span, frame: Frame) -> Result<()> {
    let shown = shown_us(span.length_us(), frame.fps);
    write!(
        g,
        "[{k}:{stream}]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,asetpts=PTS-STARTPTS,apad,atrim=duration={},asetpts=PTS-STARTPTS,volume={}.{:03}",
        seconds(shown),
        span.volume / 1000,
        span.volume % 1000
    )
    .map_err(|_| VideoError::Limit)?;
    if span.fade_in_us > 0 {
        write!(g, ",afade=t=in:st=0:d={}", seconds(span.fade_in_us)).map_err(|_| VideoError::Limit)?;
    }
    if span.fade_out_us > 0 {
        write!(g, ",afade=t=out:st={}:d={}", seconds(shown.saturating_sub(span.fade_out_us)), seconds(span.fade_out_us)).map_err(|_| VideoError::Limit)?;
    }
    Ok(())
}
/// The audio stream a part plays, if it plays one.
fn audible(media: Media<'_>, span: &Span) -> Option<u32> {
    let stream = match media {
        Media::Video(m) => m.audio.map(|(stream, _)| stream),
        Media::Audio(m) => Some(m.stream),
        Media::Still { .. } => None,
    };
    stream.filter(|_| !span.muted && span.volume > 0)
}

/// The filter graph reading input `k` from the `k`th part (the main track, then the layers),
/// ending in `[vout]` and `[aout]`.
pub(super) fn graph(frame: Frame, parts: &[(Media<'_>, &Span)], layers: &[(Media<'_>, &Span, u64)]) -> Result<String> {
    let mut g = String::new();
    let single = parts.len() == 1;
    let (main_v, main_a) = if layers.is_empty() { ("vout", "aout") } else { ("mv", "ma") };
    for (k, (media, span)) in parts.iter().enumerate() {
        let (v, a) = if single { (main_v.to_owned(), main_a.to_owned()) } else { (format!("v{k}"), format!("a{k}")) };
        picture(&mut g, k, *media, span, frame, None, &v)?;
        match audible(*media, span) {
            Some(stream) => sound(&mut g, k, stream, span, frame)?,
            None => write!(
                g,
                "anullsrc=r=48000:cl=stereo,atrim=duration={},aformat=sample_fmts=fltp:channel_layouts=stereo",
                seconds(shown_us(span.length_us(), frame.fps))
            )
            .map_err(|_| VideoError::Limit)?,
        }
        write!(g, "[{a}];").map_err(|_| VideoError::Limit)?;
    }
    let tb = format!("{}/{}", frame.fps.1, frame.fps.0);
    let mut picture_label = String::from("v0");
    let mut sound_label = String::from("a0");
    let mut length = parts.first().map(|(_, s)| shown_us(s.length_us(), frame.fps)).ok_or(VideoError::Invalid)?;
    for (k, pair) in parts.windows(2).enumerate() {
        let [(_, previous), (_, span)] = pair else { return Err(VideoError::Invalid) };
        let k = k + 1;
        let (v, a) = if k + 1 == parts.len() { (main_v.to_owned(), main_a.to_owned()) } else { (format!("x{k}"), format!("y{k}")) };
        let cross = previous.transition_us;
        if cross > 0 {
            write!(
                g,
                "[{picture_label}][v{k}]xfade=transition=fade:duration={d}:offset={offset}[{v}];[{sound_label}][a{k}]acrossfade=d={d}:c1=tri:c2=tri[{a}];",
                d = seconds(cross),
                offset = seconds(length.saturating_sub(cross)),
            )
            .map_err(|_| VideoError::Limit)?;
        } else {
            // concat hands back a microsecond timebase; xfade needs the frame rate's on both sides.
            write!(g, "[{picture_label}][{sound_label}][v{k}][a{k}]concat=n=2:v=1:a=1[c{k}][{a}];[c{k}]settb={tb}[{v}];").map_err(|_| VideoError::Limit)?;
        }
        length = length.saturating_add(shown_us(span.length_us(), frame.fps)).saturating_sub(cross);
        picture_label = v;
        sound_label = a;
    }
    if !layers.is_empty() {
        let mut total = length;
        for (_, span, at) in layers {
            total = total.max(frame_us(*at, frame.fps).saturating_add(shown_us(span.length_us(), frame.fps)));
        }
        // The main track holds its background colour until the last layer ends (a frame over, for rounding).
        write!(
            g,
            "[mv]tpad=stop_mode=add:stop_duration={}:color={}[b0];",
            seconds(total.saturating_sub(length).saturating_add(frame_us(1, frame.fps))),
            hex(frame.background)
        )
        .map_err(|_| VideoError::Limit)?;
        let mut base = 0usize;
        let mut mixed = vec![String::from("[ma]")];
        for (j, (media, span, at)) in layers.iter().enumerate() {
            let k = parts.len().saturating_add(j);
            if media.visual() {
                picture(&mut g, k, *media, span, frame, Some(*at), &format!("l{j}"))?;
                write!(g, "[b{base}][l{j}]overlay=eof_action=pass:repeatlast=0:format=yuv420[b{}];", base + 1).map_err(|_| VideoError::Limit)?;
                base += 1;
            }
            if let Some(stream) = audible(*media, span) {
                sound(&mut g, k, stream, span, frame)?;
                let delay = u128::from(*at).saturating_mul(u128::from(frame.fps.1)).saturating_mul(48_000) / u128::from(frame.fps.0).max(1);
                if delay > 0 {
                    write!(g, ",adelay=delays={delay}S:all=1").map_err(|_| VideoError::Limit)?;
                }
                write!(g, "[s{j}];").map_err(|_| VideoError::Limit)?;
                mixed.push(format!("[s{j}]"));
            }
        }
        write!(g, "[b{base}]trim=end_frame={},setpts=PTS-STARTPTS[vout];", frames(total, frame.fps)).map_err(|_| VideoError::Limit)?;
        if mixed.len() > 1 {
            write!(g, "{}amix=inputs={}:duration=longest:dropout_transition=0:normalize=0,", mixed.concat(), mixed.len()).map_err(|_| VideoError::Limit)?;
        } else {
            g.push_str("[ma]");
        }
        write!(g, "apad,atrim=duration={}[aout];", seconds(total)).map_err(|_| VideoError::Limit)?;
    }
    g.pop();
    Ok(g)
}

#[cfg(test)]
mod tests;
