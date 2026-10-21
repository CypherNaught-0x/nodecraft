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

/// One span of a staged clip on the timeline.
#[derive(Clone)]
pub struct ComposeClip {
    pub(crate) asset: VideoAsset,
    pub(crate) span: Span,
}
impl ComposeClip {
    /// The span `[in_us, out_us)` of `asset`, at full volume, with no fades.
    pub fn new(asset: VideoAsset, in_us: u64, out_us: u64) -> Result<Self> {
        if in_us >= out_us || out_us > asset.metadata().duration_us {
            return Err(VideoError::Invalid);
        }
        let span = Span { in_us, out_us, fade_in_us: 0, fade_out_us: 0, volume: 1000, muted: false, transition_us: 0 };
        Ok(Self { asset, span })
    }
    pub fn length_us(&self) -> u64 {
        self.span.length_us()
    }
    /// Fade from and to the background colour (and silence) over the clip's own ends.
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

/// The output frame and the ordered clips to stitch into it.
#[derive(Clone)]
pub struct ComposeSpec {
    pub(crate) frame: Frame,
    pub(crate) clips: Vec<ComposeClip>,
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
    pub fn new(width: u32, height: u32, fps: (u32, u32), background: [u8; 3], clips: Vec<ComposeClip>) -> Result<Self> {
        let frame = Frame { width, height, fps, background };
        let spans: Vec<Span> = clips.iter().map(|c| c.span).collect();
        validate(frame, &spans)?;
        Ok(Self { frame, clips })
    }
    /// Running time of the output: every clip (as whole frames), less the overlap of each transition.
    pub fn duration_us(&self) -> u64 {
        let total = self.clips.iter().map(|c| shown_us(c.span.length_us(), self.frame.fps)).fold(0u64, u64::saturating_add);
        total.saturating_sub(self.clips.iter().map(|c| c.span.transition_us).fold(0u64, u64::saturating_add))
    }
    pub fn clips(&self) -> &[ComposeClip] {
        &self.clips
    }
    pub(super) fn parts(&self) -> Vec<(&VideoMetadata, &Span)> {
        self.clips.iter().map(|c| (c.asset.metadata(), &c.span)).collect()
    }
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

/// Seconds with microsecond precision, as FFmpeg reads durations.
pub(super) fn seconds(us: u64) -> String {
    format!("{}.{:06}", us / 1_000_000, us % 1_000_000)
}
/// Where the clip's video stream starts reading, in seconds from the file's start.
pub(super) fn seek(m: &VideoMetadata, span: &Span) -> String {
    let offset = u64::try_from(m.stream_start.saturating_sub(m.format_start)).unwrap_or(0);
    seconds(offset.saturating_add(span.in_us))
}
/// The largest even size with the clip's display aspect that fits `width`×`height`.
fn fit(m: &VideoMetadata, width: u32, height: u32) -> (u32, u32) {
    let mut w = u64::from(m.width) * u64::from(m.sar.0.max(1));
    let mut h = u64::from(m.height) * u64::from(m.sar.1.max(1));
    if matches!(m.rotation, 90 | 270) {
        std::mem::swap(&mut w, &mut h);
    }
    let (bw, bh) = (u64::from(width), u64::from(height));
    let (fw, fh) = if w.saturating_mul(bh) > h.saturating_mul(bw) { (bw, h.saturating_mul(bw) / w.max(1)) } else { (w.saturating_mul(bh) / h.max(1), bh) };
    let even = |v: u64, cap: u32| u32::try_from(v & !1).unwrap_or(cap).clamp(2, cap);
    (even(fw, width), even(fh, height))
}
fn hex(rgb: [u8; 3]) -> String {
    format!("0x{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// The filter graph reading input `k` from part `k`, ending in `[vout]` and `[aout]`.
pub(super) fn graph(frame: Frame, parts: &[(&VideoMetadata, &Span)]) -> Result<String> {
    let mut g = String::new();
    let color = hex(frame.background);
    let rate = format!("{}/{}", frame.fps.0, frame.fps.1);
    let single = parts.len() == 1;
    for (k, (m, span)) in parts.iter().enumerate() {
        let shown = shown_us(span.length_us(), frame.fps);
        let length = seconds(shown);
        let count = frames(span.length_us(), frame.fps);
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
        let x = (frame.width.saturating_sub(w) / 2) & !1;
        let y = (frame.height.saturating_sub(h) / 2) & !1;
        let (v, a) = if single { ("vout".to_owned(), "aout".to_owned()) } else { (format!("v{k}"), format!("a{k}")) };
        write!(
            g,
            "[{k}:{stream}]setpts=PTS-STARTPTS,colorspace=ispace={p}:iprimaries={p}:itrc={t}:irange={range}:all=bt709:range=tv:format=yuv444p:fast=0:dither=none,\
             {rotate}scale={w}:{h}:flags=bicubic:in_color_matrix=bt709:out_color_matrix=bt709:in_range=tv:out_range=tv,format=yuv420p,setsar=1,\
             pad={W}:{H}:{x}:{y}:color={color},fps={rate},tpad=stop_mode=clone:stop_duration={length},trim=end_frame={count},setpts=PTS-STARTPTS",
            stream = m.stream,
            W = frame.width,
            H = frame.height,
        )
        .map_err(|_| VideoError::Limit)?;
        if span.fade_in_us > 0 {
            write!(g, ",fade=t=in:st=0:d={}:color={color}", seconds(span.fade_in_us)).map_err(|_| VideoError::Limit)?;
        }
        if span.fade_out_us > 0 {
            write!(g, ",fade=t=out:st={}:d={}:color={color}", seconds(shown.saturating_sub(span.fade_out_us)), seconds(span.fade_out_us))
                .map_err(|_| VideoError::Limit)?;
        }
        write!(g, "[{v}];").map_err(|_| VideoError::Limit)?;
        match m.audio.filter(|_| !span.muted && span.volume > 0) {
            Some((stream, _)) => {
                write!(
                    g,
                    "[{k}:{stream}]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,asetpts=PTS-STARTPTS,apad,atrim=duration={length},asetpts=PTS-STARTPTS,volume={}.{:03}",
                    span.volume / 1000,
                    span.volume % 1000
                )
                .map_err(|_| VideoError::Limit)?;
                if span.fade_in_us > 0 {
                    write!(g, ",afade=t=in:st=0:d={}", seconds(span.fade_in_us)).map_err(|_| VideoError::Limit)?;
                }
                if span.fade_out_us > 0 {
                    write!(g, ",afade=t=out:st={}:d={}", seconds(shown.saturating_sub(span.fade_out_us)), seconds(span.fade_out_us))
                        .map_err(|_| VideoError::Limit)?;
                }
            }
            None => write!(g, "anullsrc=r=48000:cl=stereo,atrim=duration={length},aformat=sample_fmts=fltp:channel_layouts=stereo")
                .map_err(|_| VideoError::Limit)?,
        }
        write!(g, "[{a}];").map_err(|_| VideoError::Limit)?;
    }
    let tb = format!("{}/{}", frame.fps.1, frame.fps.0);
    let mut picture = String::from("v0");
    let mut sound = String::from("a0");
    let mut length = parts.first().map(|(_, s)| shown_us(s.length_us(), frame.fps)).ok_or(VideoError::Invalid)?;
    for (k, pair) in parts.windows(2).enumerate() {
        let [(_, previous), (_, span)] = pair else { return Err(VideoError::Invalid) };
        let k = k + 1;
        let (v, a) = if k + 1 == parts.len() { ("vout".to_owned(), "aout".to_owned()) } else { (format!("x{k}"), format!("y{k}")) };
        let cross = previous.transition_us;
        if cross > 0 {
            write!(
                g,
                "[{picture}][v{k}]xfade=transition=fade:duration={d}:offset={offset}[{v}];[{sound}][a{k}]acrossfade=d={d}:c1=tri:c2=tri[{a}];",
                d = seconds(cross),
                offset = seconds(length.saturating_sub(cross)),
            )
            .map_err(|_| VideoError::Limit)?;
        } else {
            // concat hands back a microsecond timebase; xfade needs the frame rate's on both sides.
            write!(g, "[{picture}][{sound}][v{k}][a{k}]concat=n=2:v=1:a=1[c{k}][{a}];[c{k}]settb={tb}[{v}];").map_err(|_| VideoError::Limit)?;
        }
        length = length.saturating_add(shown_us(span.length_us(), frame.fps)).saturating_sub(cross);
        picture = v;
        sound = a;
    }
    g.pop();
    Ok(g)
}

#[cfg(test)]
mod tests;
