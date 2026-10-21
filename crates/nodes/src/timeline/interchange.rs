//! Timeline interchange (#166): a Timeline's clips as an FCPXML 1.10 document another editor
//! can open, with each media file named by a path relative to the document. Nothing is
//! rendered: the clips are described as they are set, and every setting the format can't carry
//! is listed as a [`Loss`] rather than dropped unseen. The supported settings are in
//! `docs/timeline-interchange.md`.
//!
//! Timing: everything is placed on whole frames of the Timeline's rate. The main track plays in
//! order; a cross transition becomes a dissolve centred on the cut, so each clip lends half of
//! the overlap to it, as editors that read FCPXML expect. Clips on other tracks are connected to
//! whatever is under them on the main track (a gap past its end), on lanes over it for pictures
//! and under it for sound.
use super::{Input, Timeline, TrackKind, Wire};
use nodecraft_types::{Error, ErrorKind};
use serde::Serialize;
use std::fmt::Write as _;

/// The format's name, as commands and the archive say it.
pub const FORMAT: &str = "fcpxml";
/// The FCPXML version written.
pub const VERSION: &str = "1.10";
/// The quietest volume written, in decibels: a muted clip, or one at volume 0.
pub const SILENT_DB: f64 = -96.0;

/// What a source file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
    Still,
}

/// One media file the document names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Shown in the other editor's browser.
    pub name: String,
    /// The file's path relative to the document, `/`-separated (e.g. `media/01-fox.mp4`).
    pub file: String,
    pub kind: MediaKind,
    /// Whether a video has a sound track (a sound file always has; a still never).
    pub sound: bool,
    /// How long the media plays, when it says.
    pub duration_us: Option<u64>,
}

/// A Timeline to describe: its frame, its clips and the media wired into it now.
#[derive(Clone, Debug)]
pub struct Sequence<'a> {
    pub name: &'a str,
    pub width: u32,
    pub height: u32,
    /// Frames per second (the Timeline's rates are whole numbers).
    pub fps: u32,
    pub background: [u8; 3],
    pub timeline: &'a Timeline,
    /// Each wired input, in input order.
    pub wires: &'a [Wire],
    /// Each wire's media, an index into `sources`; None when the wire carries no media yet.
    pub media: &'a [Option<usize>],
    pub sources: &'a [Source],
}

/// A setting the document doesn't carry as NodeCraft plays it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Loss {
    /// The clip it concerns, when it concerns one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    /// What NodeCraft does that the document doesn't, in a few words.
    pub feature: &'static str,
    /// What the other editor gets instead.
    pub detail: String,
}

/// The document, what it leaves out and which sources it names.
#[derive(Clone, Debug, PartialEq)]
pub struct Interchange {
    pub xml: String,
    pub losses: Vec<Loss>,
    /// Indexes into the sequence's sources, each once, in the order the document lists them.
    pub used: Vec<usize>,
    /// The sequence's length, in frames.
    pub frames: u64,
}

fn invalid(message: String) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Microseconds to whole frames at `fps`, rounded to the nearest; and whether that was exact.
fn frames(us: u64, fps: u32) -> (u64, bool) {
    let scaled = u128::from(us).saturating_mul(u128::from(fps));
    let whole = scaled.saturating_add(500_000) / 1_000_000;
    (u64::try_from(whole).unwrap_or(u64::MAX), scaled % 1_000_000 == 0)
}

/// Microseconds to the whole frames they hold, rounded down.
fn floor_frames(us: u64, fps: u32) -> u64 {
    u64::try_from(u128::from(us).saturating_mul(u128::from(fps)) / 1_000_000).unwrap_or(u64::MAX)
}

/// A time on the sequence's frame grid, as FCPXML writes one.
fn time(frames: u64, fps: u32) -> String {
    if frames == 0 { "0s".into() } else { format!("{frames}/{fps}s") }
}

/// Microseconds exactly, for a source's own length.
fn exact(us: u64) -> String {
    if us == 0 { "0s".into() } else { format!("{us}/1000000s") }
}

/// `text` safe inside a double-quoted XML attribute. Control characters XML 1.0 can't carry
/// become spaces.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() || matches!(c, '\u{FFFE}' | '\u{FFFF}') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A relative path as a URL reference: each byte outside the unreserved set percent-encoded,
/// `/` kept between segments.
pub fn relative_url(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// A clip's volume in decibels; `None` when it plays as recorded.
fn decibels(volume: u16, muted: bool) -> Option<f64> {
    if muted || volume == 0 {
        return Some(SILENT_DB);
    }
    (volume != 1000).then(|| (20.0 * (f64::from(volume) / 1000.0).log10()).max(SILENT_DB))
}

/// One clip, placed: its source, where it starts in it and how long it plays, in frames.
struct Placed<'a> {
    id: &'a str,
    number: usize,
    source: usize,
    kind: MediaKind,
    in_f: u64,
    len_f: u64,
    volume: Option<f64>,
}

/// An item on the main track, as written: where it starts on the sequence, where in its media,
/// how long it is.
struct Spined<'a> {
    clip: Option<Placed<'a>>,
    offset: u64,
    start: u64,
    duration: u64,
    /// The dissolve into the next item, starting at `.0`, `.1` frames long.
    dissolve: Option<(u64, u64)>,
}

/// Describes `sequence` as an FCPXML document.
pub fn fcpxml(sequence: &Sequence<'_>) -> Result<Interchange, Error> {
    let fps = sequence.fps;
    if fps == 0 {
        return Err(invalid("the timeline's frame rate is 0".into()));
    }
    let timeline = sequence.timeline;
    let listed = timeline.with_sources(sequence.wires);
    let mut losses = Vec::new();
    for clip in &listed.clips {
        if !clip.enabled {
            losses.push(Loss { clip: Some(clip.id.clone()), feature: "disabled clip", detail: "left out, as a render leaves it out".into() });
        } else if !Timeline::is_wired(clip, sequence.wires) {
            losses.push(Loss { clip: Some(clip.id.clone()), feature: "unconnected clip", detail: "left out: its media isn't wired in".into() });
        }
    }
    let mut used: Vec<usize> = Vec::new();
    // The furthest frame any clip plays of each source, for media whose length isn't known.
    let mut reach: Vec<u64> = vec![0; sequence.sources.len()];
    let (mut main, mut layers) = (Vec::new(), Vec::new());
    let resolved = timeline.resolve(sequence.wires);
    for (n, (input, clip)) in resolved.iter().enumerate() {
        let number = listed.clips.iter().position(|c| c.id == clip.id).unwrap_or(n).saturating_add(1);
        let source_index = sequence
            .media
            .get(*input)
            .copied()
            .flatten()
            .ok_or_else(|| invalid(format!("clip {number} has no media yet: run the node it comes from, or disconnect it")))?;
        let source = sequence.sources.get(source_index).ok_or_else(|| invalid(format!("clip {number}'s media isn't listed")))?;
        let out_us = match (clip.out_us, source.duration_us, source.kind) {
            (Some(out), Some(length), MediaKind::Video | MediaKind::Audio) if out > length => {
                return Err(invalid(format!("clip {number} ends after its media does")));
            }
            (Some(out), ..) => out,
            (None, Some(length), _) => length,
            (None, None, _) => return Err(invalid(format!("clip {number}'s media doesn't say how long it is: set where the clip ends"))),
        };
        if clip.in_us >= out_us {
            return Err(invalid(format!("clip {number} starts after its media ends")));
        }
        let (in_f, in_exact) = frames(clip.in_us, fps);
        let (mut out_f, out_exact) = frames(out_us, fps);
        // Rounding never runs a clip past the end of its media: the last whole frame it has.
        let last = match (source.duration_us, source.kind) {
            (Some(length), MediaKind::Video | MediaKind::Audio) => Some(floor_frames(length, fps)),
            _ => None,
        };
        if let Some(last) = last {
            out_f = out_f.min(last);
        }
        let (at_f, at_exact) = frames(clip.at_us, fps);
        let (transition_f, transition_exact) = frames(clip.transition_us, fps);
        let len_f = out_f.saturating_sub(in_f);
        if len_f == 0 {
            return Err(invalid(format!("clip {number} is shorter than a frame")));
        }
        if !(in_exact && out_exact && at_exact && transition_exact) {
            losses.push(Loss {
                clip: Some(clip.id.clone()),
                feature: "sub-frame timing",
                detail: format!("trims, start and transition rounded to whole frames at {fps} fps"),
            });
        }
        if clip.fade_in_us != 0 || clip.fade_out_us != 0 {
            losses.push(Loss { clip: Some(clip.id.clone()), feature: "fade", detail: "not written: add the fades in the other editor".into() });
        }
        let audible = source.kind == MediaKind::Audio || (source.kind == MediaKind::Video && source.sound);
        let volume = if audible { decibels(clip.volume, clip.muted) } else { None };
        if !used.contains(&source_index) {
            used.push(source_index);
        }
        if let Some(slot) = reach.get_mut(source_index) {
            *slot = (*slot).max(out_f);
        }
        let placed = Placed { id: &clip.id, number, source: source_index, kind: source.kind, in_f, len_f, volume };
        if clip.track == 0 {
            main.push((placed, transition_f));
        } else {
            layers.push((placed, clip.track, at_f));
        }
    }

    // The main track: back to back, each transition shared out around its cut.
    let mut spine: Vec<Spined<'_>> = Vec::new();
    let (mut at, mut lead) = (0u64, 0u64);
    let count = main.len();
    for (k, (placed, transition)) in main.into_iter().enumerate() {
        let transition = if k.saturating_add(1) == count { 0 } else { transition };
        let half = transition / 2;
        let tail = transition.saturating_sub(half);
        let duration = placed
            .len_f
            .checked_sub(lead)
            .and_then(|d| d.checked_sub(tail))
            .filter(|d| *d > 0)
            .ok_or_else(|| invalid(format!("clip {} is shorter than its transitions at {fps} fps", placed.number)))?;
        let offset = at.saturating_add(lead);
        let start = placed.in_f.saturating_add(lead);
        let end = at.saturating_add(placed.len_f);
        let dissolve = (transition > 0).then(|| (end.saturating_sub(transition), transition));
        spine.push(Spined { clip: Some(placed), offset, start, duration, dissolve });
        at = end.saturating_sub(transition);
        lead = half;
    }
    let main_end = at;
    let total = layers.iter().map(|(p, _, at_f)| at_f.saturating_add(p.len_f)).fold(main_end, u64::max);
    if total == 0 {
        return Err(invalid("the timeline has no clips to export: connect a video, sound or picture, or enable a clip".into()));
    }
    if total > main_end {
        spine.push(Spined { clip: None, offset: main_end, start: main_end, duration: total.saturating_sub(main_end), dissolve: None });
    }
    if sequence.background != [0, 0, 0] {
        losses.push(Loss { clip: None, feature: "background colour", detail: "not written: the other editor shows black behind the picture".into() });
    }

    // Each connected clip, under the main-track item it starts over.
    let mut connected: Vec<Vec<(Placed<'_>, i32, u64)>> = (0..spine.len()).map(|_| Vec::new()).collect();
    for (placed, track, at_f) in layers {
        let lane = lane(&listed, track)?;
        let parent = spine.iter().position(|s| s.offset <= at_f && at_f < s.offset.saturating_add(s.duration)).unwrap_or(spine.len().saturating_sub(1));
        let item = spine.get(parent).ok_or_else(|| invalid("a clip has nothing to connect to".into()))?;
        let local = item.start.saturating_add(at_f.saturating_sub(item.offset));
        if let Some(list) = connected.get_mut(parent) {
            list.push((placed, lane, local));
        }
    }

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n");
    let _ = writeln!(xml, "<fcpxml version=\"{VERSION}\">");
    xml.push_str("  <resources>\n");
    let _ = writeln!(xml, "    <format id=\"r1\" frameDuration=\"1/{fps}s\" width=\"{}\" height=\"{}\"/>", sequence.width, sequence.height);
    for (n, index) in used.iter().enumerate() {
        let source = sequence.sources.get(*index).ok_or_else(|| invalid("a clip's media isn't listed".into()))?;
        let (video, audio) = match source.kind {
            MediaKind::Video => (1, u8::from(source.sound)),
            MediaKind::Audio => (0, 1),
            MediaKind::Still => (1, 0),
        };
        let duration = match (source.kind, source.duration_us) {
            (MediaKind::Still, _) => "0s".into(),
            (_, Some(length)) => exact(length),
            (_, None) => {
                losses.push(Loss {
                    clip: None,
                    feature: "unknown media length",
                    detail: format!("{} is written as long as the furthest clip plays it; the other editor may find more", source.name),
                });
                time(reach.get(*index).copied().unwrap_or(0), fps)
            }
        };
        let _ = writeln!(
            xml,
            "    <asset id=\"{}\" name=\"{}\" start=\"0s\" duration=\"{duration}\" hasVideo=\"{video}\" hasAudio=\"{audio}\">",
            asset_id(n),
            escape(&source.name)
        );
        let _ = writeln!(xml, "      <media-rep kind=\"original-media\" src=\"{}\"/>", escape(&relative_url(&source.file)));
        xml.push_str("    </asset>\n");
    }
    xml.push_str("  </resources>\n  <library>\n");
    let name = escape(sequence.name);
    let _ = writeln!(xml, "    <event name=\"{name}\">\n      <project name=\"{name}\">");
    let _ = writeln!(
        xml,
        "        <sequence format=\"r1\" duration=\"{}\" tcStart=\"0s\" tcFormat=\"NDF\" audioLayout=\"stereo\" audioRate=\"48k\">\n          <spine>",
        time(total, fps)
    );
    let ref_of = |source: usize| used.iter().position(|u| *u == source).map(asset_id).unwrap_or_default();
    for (item, children) in spine.iter().zip(&connected) {
        let indent = "            ";
        match &item.clip {
            None => {
                let _ = write!(
                    xml,
                    "{indent}<gap name=\"Gap\" offset=\"{}\" start=\"{}\" duration=\"{}\"",
                    time(item.offset, fps),
                    time(item.start, fps),
                    time(item.duration, fps)
                );
            }
            Some(placed) => {
                let _ = write!(
                    xml,
                    "{indent}<{} ref=\"{}\" name=\"{}\" offset=\"{}\" start=\"{}\" duration=\"{}\"",
                    element(placed.kind),
                    ref_of(placed.source),
                    escape(placed.id),
                    time(item.offset, fps),
                    time(item.start, fps),
                    time(item.duration, fps)
                );
            }
        }
        let volume = item.clip.as_ref().and_then(|p| p.volume);
        if children.is_empty() && volume.is_none() {
            xml.push_str("/>\n");
        } else {
            xml.push_str(">\n");
            if let Some(db) = volume {
                let _ = writeln!(xml, "{indent}  <adjust-volume amount=\"{db:.2}dB\"/>");
            }
            for (placed, lane, local) in children {
                let _ = write!(
                    xml,
                    "{indent}  <{} ref=\"{}\" lane=\"{lane}\" name=\"{}\" offset=\"{}\" start=\"{}\" duration=\"{}\"",
                    element(placed.kind),
                    ref_of(placed.source),
                    escape(placed.id),
                    time(*local, fps),
                    time(placed.in_f, fps),
                    time(placed.len_f, fps)
                );
                match placed.volume {
                    None => xml.push_str("/>\n"),
                    Some(db) => {
                        let _ = write!(xml, ">\n{indent}    <adjust-volume amount=\"{db:.2}dB\"/>\n{indent}  </{}>\n", element(placed.kind));
                    }
                }
            }
            let _ = writeln!(xml, "{indent}</{}>", item.clip.as_ref().map_or("gap", |p| element(p.kind)));
        }
        if let Some((offset, duration)) = item.dissolve {
            let _ = writeln!(xml, "{indent}<transition name=\"Cross Dissolve\" offset=\"{}\" duration=\"{}\"/>", time(offset, fps), time(duration, fps));
        }
    }
    xml.push_str("          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n");
    Ok(Interchange { xml, losses, used, frames: total })
}

fn asset_id(n: usize) -> String {
    format!("r{}", n.saturating_add(2))
}

/// The element a clip of `kind` is written as: a still is a `video` held for its length.
fn element(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Still => "video",
        MediaKind::Video | MediaKind::Audio => "asset-clip",
    }
}

/// The lane track `track` is written on: picture tracks over the main one (1 up), sound tracks
/// under it (-1 down), each in track order.
fn lane(timeline: &Timeline, track: u8) -> Result<i32, Error> {
    let kind = timeline.kind_of(track).ok_or_else(|| invalid("a clip is on a track the timeline doesn't have".into()))?;
    let before = (1..=track).filter(|t| timeline.kind_of(*t) == Some(kind)).count();
    let lane = i32::try_from(before).map_err(|_| invalid("too many tracks".into()))?;
    Ok(if kind == TrackKind::Audio { -lane } else { lane })
}

/// The kind of media `input` takes, for a source wired into it.
pub fn kind_of(input: Input) -> MediaKind {
    match input {
        Input::Clips => MediaKind::Video,
        Input::Audio => MediaKind::Audio,
        Input::Stills => MediaKind::Still,
    }
}

#[cfg(test)]
mod tests;
