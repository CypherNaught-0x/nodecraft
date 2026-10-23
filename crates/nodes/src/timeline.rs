//! The Timeline kind (#109, #576): every video wired into `clips` and picture into `stills`
//! becomes a clip on the main track, played in order, each trimmed, faded, crossed into the next
//! and mixed at its own volume; every sound wired into `audio` a clip on an audio track. More
//! tracks hold clips at their own time: video tracks draw over the main track (a higher track on
//! top), audio tracks mix under it. The output is one rendered video. The clips live in the node's `config["nodecraft.timeline"]` as JSON
//! (`{"clips": [...]}`, first first; the format is [`Timeline`]). The host checks them where they
//! are set (`timeline.*` commands) and again, against each video's length, before it renders;
//! this kind only carries them, with the node each connected video came from, so a clip stays
//! with its video when wires are reordered. Captions (#574) are stored with them: text drawn over
//! the picture between two times (see [`caption`]).
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, NodeMigration, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Choice, Error, ErrorKind, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Unsigned, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

pub mod caption;
pub mod provenance;
pub use caption::{Caption, MAX_CAPTIONS};

/// The kind's id.
pub const ID: &str = "video.timeline";
/// The config key holding the clips.
pub const CONFIG: &str = "nodecraft.timeline";
/// The parameter the planned job carries the clips in (not a control).
pub const SPEC_PARAM: &str = "nodecraft.timeline";
/// The parameter the planned job carries each input's origin node in (not a control).
pub const SOURCES_PARAM: &str = "nodecraft.timeline.sources";
/// No clips, as stored.
pub const EMPTY: &str = r#"{"clips":[]}"#;
/// The parameter the planned job carries each input's port in (not a control).
pub const PORTS_PARAM: &str = "nodecraft.timeline.ports";
/// The input port for videos.
pub const INPUT: &str = "clips";
/// The input port for sound files.
pub const AUDIO_INPUT: &str = "audio";
/// The input port for still pictures.
pub const STILLS_INPUT: &str = "stills";
/// The output port.
pub const OUTPUT: &str = "video";
/// The frame's edge range and defaults, in pixels (even: the encoder needs it).
pub const MIN_FRAME: u64 = 16;
pub const MAX_FRAME: u64 = 4096;
pub const DEFAULT_WIDTH: u64 = 1280;
pub const DEFAULT_HEIGHT: u64 = 720;
/// The frame rates offered, frames per second.
pub const RATES: [&str; 5] = ["24", "25", "30", "50", "60"];
/// Most clips on a timeline, and most stored bytes.
pub const MAX_CLIPS: usize = 32;
pub const MAX_ENCODED_BYTES: usize = 128 * 1024;
/// Longest time anywhere on a timeline: an hour, in microseconds.
pub const MAX_US: u64 = 3_600_000_000;
/// Shortest cross transition; shorter ones are a cut (0).
pub const MIN_TRANSITION_US: u64 = 10_000;
/// Loudest clip volume, in thousandths (4000 is four times as loud).
pub const MAX_VOLUME: u16 = 4000;
/// Longest clip id, in bytes.
pub const MAX_ID_BYTES: usize = 64;
/// Most tracks besides the main one.
pub const MAX_TRACKS: usize = 8;
/// How long a still shows until its length is set.
pub const DEFAULT_STILL_US: u64 = 3_000_000;
/// The identity placement, in thousandths: centred, at the size that fits the frame, opaque.
pub const CENTRE: i32 = 500;
pub const UNIT: u16 = 1000;
/// Where a clip's centre may go, in thousandths of the frame (a clip may sit partly outside it).
pub const MIN_POS: i32 = -1000;
pub const MAX_POS: i32 = 2000;
/// A clip's size range, in thousandths of the size that fits the frame.
pub const MIN_SCALE: u16 = 10;
pub const MAX_SCALE: u16 = 4000;
/// A clip's playback rate range, in thousandths (500 is half speed).
pub const MIN_SPEED: u16 = 250;
pub const MAX_SPEED: u16 = 4000;
/// The render qualities offered, worst first.
pub const QUALITIES: [&str; 3] = ["draft", "standard", "high"];

/// Which port a clip's media is wired into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Input {
    #[default]
    Clips,
    Audio,
    Stills,
}
impl Input {
    pub const ALL: [Self; 3] = [Self::Clips, Self::Audio, Self::Stills];
    pub fn port(self) -> &'static str {
        match self {
            Self::Clips => INPUT,
            Self::Audio => AUDIO_INPUT,
            Self::Stills => STILLS_INPUT,
        }
    }
    pub fn from_port(port: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.port() == port)
    }
    /// Whether this media has a picture (and goes on a video track).
    pub fn visual(self) -> bool {
        self != Self::Audio
    }
    fn is_default(&self) -> bool {
        *self == Self::Clips
    }
}

/// One wired input: the port it comes in on and the node it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub input: Input,
    pub origin: String,
}
impl Wire {
    pub fn new(input: Input, origin: impl Into<String>) -> Self {
        Self { input, origin: origin.into() }
    }
    /// A video wired into `clips`.
    pub fn video(origin: impl Into<String>) -> Self {
        Self::new(Input::Clips, origin)
    }
}

/// What a track beyond the main one holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    /// Pictures drawn over the tracks below it.
    Video,
    /// Sound mixed under the picture.
    Audio,
}

/// A track beyond the main one. Track 0, the main track, is implicit: pictures played in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub kind: TrackKind,
}

/// The clips (first first; on the main track that is playing order), the tracks beyond the
/// main one, track 1 first, and the captions drawn over them all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub clips: Vec<Clip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captions: Vec<Caption>,
}

/// One clip: a stretch of the `nth` video, sound or picture wired from node `origin` into `input`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: String,
    /// The id of the node the media comes from.
    pub origin: String,
    /// The port it is wired into.
    #[serde(default, skip_serializing_if = "Input::is_default")]
    pub input: Input,
    /// The track it is on: 0 is the main track, 1 the first in `Timeline::tracks`.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub track: u8,
    /// Where it starts on a track beyond the main one, in microseconds from the start.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at_us: u64,
    /// Which of that node's videos, from 0.
    #[serde(default)]
    pub nth: u32,
    /// Where in the video the clip starts, in microseconds.
    #[serde(default)]
    pub in_us: u64,
    /// Where it ends; the end of the video when unset. A still's length (from `in_us`, which is 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_us: Option<u64>,
    #[serde(default)]
    pub fade_in_us: u64,
    #[serde(default)]
    pub fade_out_us: u64,
    /// Thousandths: 1000 plays the clip's sound as it is.
    #[serde(default = "unity")]
    pub volume: u16,
    #[serde(default)]
    pub muted: bool,
    /// A cross transition into the next clip; 0 cuts.
    #[serde(default)]
    pub transition_us: u64,
    /// A disabled clip stays on the timeline but isn't rendered.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Where the clip's centre is in the frame, in thousandths of its width and height.
    #[serde(default = "centre", skip_serializing_if = "is_centre")]
    pub pos_x: i32,
    #[serde(default = "centre", skip_serializing_if = "is_centre")]
    pub pos_y: i32,
    /// Its size, in thousandths of the size at which its (cropped) picture fits the frame.
    #[serde(default = "unity", skip_serializing_if = "is_unity")]
    pub scale: u16,
    /// How much of the picture is cut from each edge, in thousandths of its width or height.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub crop_left: u16,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub crop_top: u16,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub crop_right: u16,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub crop_bottom: u16,
    /// Clockwise turn about its centre, in degrees from -180 to 180.
    #[serde(default, skip_serializing_if = "is_zero_i16")]
    pub rotation: i16,
    /// Thousandths: 1000 is opaque.
    #[serde(default = "unity", skip_serializing_if = "is_unity")]
    pub opacity: u16,
    /// Playback rate, in thousandths: 2000 plays twice as fast, so the clip lasts half as long.
    #[serde(default = "unity", skip_serializing_if = "is_unity")]
    pub speed: u16,
}

/// Where a clip sits in the output frame (see the fields of [`Clip`] with the same names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub pos_x: i32,
    pub pos_y: i32,
    pub scale: u16,
    pub crop: [u16; 4],
    pub rotation: i16,
    pub opacity: u16,
}
impl Placement {
    /// Centred, fitted, uncropped, upright and opaque.
    pub const IDENTITY: Self = Self { pos_x: CENTRE, pos_y: CENTRE, scale: UNIT, crop: [0; 4], rotation: 0, opacity: UNIT };
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

fn is_zero_i16(v: &i16) -> bool {
    *v == 0
}

fn unity() -> u16 {
    UNIT
}

fn is_unity(v: &u16) -> bool {
    *v == UNIT
}

fn centre() -> i32 {
    CENTRE
}

fn is_centre(v: &i32) -> bool {
    *v == CENTRE
}

fn yes() -> bool {
    true
}

impl Clip {
    /// A whole video, as wired in: no trim, fade or transition.
    pub fn whole(id: String, origin: String, nth: u32) -> Self {
        Self {
            id,
            origin,
            input: Input::Clips,
            track: 0,
            at_us: 0,
            nth,
            in_us: 0,
            out_us: None,
            fade_in_us: 0,
            fade_out_us: 0,
            volume: unity(),
            muted: false,
            transition_us: 0,
            enabled: true,
            pos_x: CENTRE,
            pos_y: CENTRE,
            scale: UNIT,
            crop_left: 0,
            crop_top: 0,
            crop_right: 0,
            crop_bottom: 0,
            rotation: 0,
            opacity: UNIT,
            speed: UNIT,
        }
    }

    /// Whole media wired into `input`, on `track`: a still shows for [`DEFAULT_STILL_US`].
    pub fn wired(id: String, input: Input, origin: String, nth: u32, track: u8) -> Self {
        let out_us = (input == Input::Stills).then_some(DEFAULT_STILL_US);
        Self { input, track, out_us, ..Self::whole(id, origin, nth) }
    }

    /// The length of media the clip plays, when its end is set.
    pub fn length_us(&self) -> Option<u64> {
        self.out_us.map(|out| out.saturating_sub(self.in_us))
    }

    /// How long the clip lasts on the timeline, at its speed: `media_us` of its media.
    pub fn played(&self, media_us: u64) -> u64 {
        played_us(media_us, self.speed)
    }

    /// How long it lasts on the timeline, when its end is set.
    pub fn played_us(&self) -> Option<u64> {
        self.length_us().map(|length| self.played(length))
    }

    /// Where it sits in the frame.
    pub fn placement(&self) -> Placement {
        Placement {
            pos_x: self.pos_x,
            pos_y: self.pos_y,
            scale: self.scale,
            crop: [self.crop_left, self.crop_top, self.crop_right, self.crop_bottom],
            rotation: self.rotation,
            opacity: self.opacity,
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        if !valid_id(&self.id) {
            return Err(bad("a clip id is 1 to 64 letters, digits, - or _"));
        }
        if !valid_id(&self.origin) {
            return Err(bad("a clip's origin is the id of the node its video comes from"));
        }
        let times = [self.in_us, self.out_us.unwrap_or(0), self.fade_in_us, self.fade_out_us, self.transition_us, self.at_us];
        if times.iter().any(|t| *t > MAX_US) {
            return Err(bad("a clip's times are within an hour"));
        }
        if self.out_us.is_some_and(|out| out <= self.in_us) {
            return Err(bad("a clip ends after it starts"));
        }
        if !(MIN_SPEED..=MAX_SPEED).contains(&self.speed) {
            return Err(bad("a clip's speed is 250 to 4000 thousandths"));
        }
        if self.input == Input::Stills && self.speed != UNIT {
            return Err(bad("a still has no speed: set its length instead"));
        }
        if self.played_us().is_some_and(|length| self.fade_in_us.saturating_add(self.fade_out_us) > length) {
            return Err(bad("a clip's fades are no longer than the clip"));
        }
        if self.transition_us != 0 && self.transition_us < MIN_TRANSITION_US {
            return Err(bad("a transition is 0 (a cut) or at least 10 ms"));
        }
        if self.volume > MAX_VOLUME {
            return Err(bad("a clip's volume is 0 to 4000 thousandths"));
        }
        if self.input == Input::Stills && (self.in_us != 0 || self.out_us.is_none()) {
            return Err(bad("a still starts at 0 and has a length (out_us)"));
        }
        if self.track == 0 && self.at_us != 0 {
            return Err(bad("the main track plays in order: at_us is for clips on other tracks"));
        }
        if self.track != 0 && self.transition_us != 0 {
            return Err(bad("only clips on the main track cross into the next"));
        }
        self.check_placement()
    }

    fn check_placement(&self) -> Result<(), Error> {
        let placed = self.placement();
        if placed == Placement::IDENTITY {
            return Ok(());
        }
        if !self.input.visual() {
            return Err(bad("a sound has no place in the frame"));
        }
        if ![placed.pos_x, placed.pos_y].iter().all(|p| (MIN_POS..=MAX_POS).contains(p)) {
            return Err(bad("a clip's position is -1000 to 2000 thousandths of the frame"));
        }
        if !(MIN_SCALE..=MAX_SCALE).contains(&placed.scale) {
            return Err(bad("a clip's scale is 10 to 4000 thousandths"));
        }
        let [left, top, right, bottom] = placed.crop.map(u32::from);
        if left + right >= 900 || top + bottom >= 900 {
            return Err(bad("a clip's crop leaves at least a tenth of its width and height"));
        }
        if !(-180..=180).contains(&placed.rotation) {
            return Err(bad("a clip's rotation is -180 to 180 degrees"));
        }
        if placed.opacity > UNIT {
            return Err(bad("a clip's opacity is 0 to 1000 thousandths"));
        }
        Ok(())
    }
}

/// How long `media_us` of media lasts played at `speed` thousandths (rounded down, at least 1 µs).
pub fn played_us(media_us: u64, speed: u16) -> u64 {
    if speed == UNIT || media_us == 0 {
        return media_us;
    }
    let played = u128::from(media_us) * u128::from(UNIT) / u128::from(speed.max(1));
    u64::try_from(played).unwrap_or(u64::MAX).max(1)
}

fn bad(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Whether `id` can name a clip or a node: 1 to 64 letters, digits, `-` or `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Timeline {
    /// Parses and checks stored or planned clips. Rejects oversized input before decoding it.
    pub fn parse(json: &str) -> Result<Self, Error> {
        if json.len() > MAX_ENCODED_BYTES {
            return Err(bad("the timeline is too large"));
        }
        let timeline: Self = serde_json::from_str(json).map_err(|_| bad("the timeline can't be read"))?;
        timeline.check()?;
        Ok(timeline)
    }

    pub fn encode(&self) -> Result<String, Error> {
        self.check()?;
        let json = serde_json::to_string(self).map_err(|_| bad("the timeline can't be stored"))?;
        if json.len() > MAX_ENCODED_BYTES { Err(bad("the timeline is too large")) } else { Ok(json) }
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.clips.len() > MAX_CLIPS {
            return Err(bad("a timeline has at most 32 clips"));
        }
        if self.tracks.len() > MAX_TRACKS {
            return Err(bad("a timeline has at most 8 tracks besides the main one"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for clip in &self.clips {
            clip.check()?;
            match self.kind_of(clip.track) {
                None => return Err(bad("a clip is on a track the timeline doesn't have")),
                Some(TrackKind::Audio) if clip.input.visual() => return Err(bad("a video or still goes on a video track")),
                Some(TrackKind::Video) if !clip.input.visual() => return Err(bad("a sound goes on an audio track")),
                _ => {}
            }
            if !ids.insert(clip.id.as_str()) {
                return Err(Error::new(ErrorKind::Conflict, "two clips have the same id"));
            }
        }
        if self.captions.len() > MAX_CAPTIONS {
            return Err(bad("a timeline has at most 64 captions"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for caption in &self.captions {
            caption.check()?;
            if !ids.insert(caption.id.as_str()) {
                return Err(Error::new(ErrorKind::Conflict, "two captions have the same id"));
            }
        }
        Ok(())
    }

    /// What track `track` holds: 0 is the main video track.
    pub fn kind_of(&self, track: u8) -> Option<TrackKind> {
        match track {
            0 => Some(TrackKind::Video),
            n => self.tracks.get(usize::from(n).checked_sub(1)?).map(|t| t.kind),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Every clip the node shows, given each connected input (`wires`, in input order; an unknown
    /// origin is empty): the stored ones, then a whole clip for each connected input no stored clip
    /// names, in input order. Videos and stills go on the main track; sounds on the first audio
    /// track, which is added when there is none.
    pub fn with_sources(&self, wires: &[Wire]) -> Self {
        let keys = source_keys(wires);
        let mut taken: std::collections::BTreeSet<String> = self.clips.iter().map(|c| c.id.clone()).collect();
        let mut shown = self.clone();
        for (input, origin, nth) in keys {
            if self.names(input, &origin, nth) {
                continue;
            }
            let track = if input.visual() {
                0
            } else {
                match shown.tracks.iter().position(|t| t.kind == TrackKind::Audio) {
                    Some(at) => at,
                    None if shown.tracks.len() < MAX_TRACKS => {
                        shown.tracks.push(Track { kind: TrackKind::Audio });
                        shown.tracks.len().saturating_sub(1)
                    }
                    None => continue,
                }
                .saturating_add(1)
            };
            let base = auto_id(input, &origin, nth);
            let mut id = base.clone();
            let mut n = 2u32;
            while taken.contains(&id) {
                id = format!("{}-{n}", base.get(..base.len().min(MAX_ID_BYTES.saturating_sub(12))).unwrap_or("clip"));
                n = n.saturating_add(1);
            }
            taken.insert(id.clone());
            shown.clips.push(Clip::wired(id, input, origin, nth, u8::try_from(track).unwrap_or(u8::MAX)));
        }
        shown
    }

    /// Whether a wired sound has no clip and no room for the sound track it would get: every
    /// extra track is a picture track. The run refuses it rather than drop the sound unseen.
    pub fn unplaced_sound(&self, wires: &[Wire]) -> bool {
        let shown = self.with_sources(wires);
        source_keys(wires).into_iter().any(|(input, origin, nth)| input == Input::Audio && !shown.names(input, &origin, nth))
    }

    /// The clips to render, each with the input it plays: the enabled clips whose media is
    /// connected, track by track (the main track first, in playing order; then each track in
    /// turn, a higher one on top). The main track's last clip plays out with no transition.
    pub fn resolve(&self, wires: &[Wire]) -> Vec<(usize, Clip)> {
        let keys = source_keys(wires);
        let mut out: Vec<(usize, Clip)> = self
            .with_sources(wires)
            .clips
            .into_iter()
            .filter(|c| c.enabled)
            .filter_map(|c| keys.iter().position(|(i, o, n)| *i == c.input && *o == c.origin && *n == c.nth).map(|input| (input, c)))
            .collect();
        out.sort_by_key(|(_, c)| c.track);
        if let Some((_, last)) = out.iter_mut().rev().find(|(_, c)| c.track == 0) {
            last.transition_us = 0;
        }
        out
    }

    /// Whether a stored clip plays the `nth` media wired from `origin` into `input`.
    fn names(&self, input: Input, origin: &str, nth: u32) -> bool {
        self.clips.iter().any(|c| c.input == input && c.origin == origin && c.nth == nth)
    }

    /// Whether `clip`'s media is connected now.
    pub fn is_wired(clip: &Clip, wires: &[Wire]) -> bool {
        source_keys(wires).iter().any(|(i, o, n)| *i == clip.input && *o == clip.origin && *n == clip.nth)
    }
}

/// Each wire's (input, origin, nth from that origin into that input). Unknown origins count as distinct.
fn source_keys(wires: &[Wire]) -> Vec<(Input, String, u32)> {
    let mut seen: std::collections::BTreeMap<(Input, &str), u32> = std::collections::BTreeMap::new();
    wires
        .iter()
        .enumerate()
        .map(|(index, wire)| {
            if !valid_id(&wire.origin) {
                return (wire.input, format!("input{index}"), 0);
            }
            let nth = seen.entry((wire.input, wire.origin.as_str())).or_insert(0);
            let key = (wire.input, wire.origin.clone(), *nth);
            *nth = nth.saturating_add(1);
            key
        })
        .collect()
}

/// The clip id media from `origin` into `input` gets until it is stored: stable while that wire stays.
pub fn auto_id(input: Input, origin: &str, nth: u32) -> String {
    let mark = match input {
        Input::Clips => "",
        Input::Audio => "a",
        Input::Stills => "s",
    };
    let id = format!("{origin}-{mark}{nth}");
    if valid_id(&id) { id } else { format!("clip-{mark}{nth}") }
}

/// The clips `node` carries, as stored (no clips when it has none).
pub fn stored(node: &Node) -> Result<&str, Error> {
    match node.config.get(CONFIG) {
        None => Ok(EMPTY),
        Some(Value::Text(json)) => Ok(json),
        Some(_) => Err(invalid("the node's timeline can't be read")),
    }
}

fn edge(name: &str, label: &str, default: u64) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some("In pixels, an even number from 16 to 4096.".into()),
        control: ParamControl::Unsigned { min: Some(Unsigned(MIN_FRAME)), max: Some(Unsigned(MAX_FRAME)), step: Unsigned(2) },
        required: true,
        default: Some(Value::Unsigned(Unsigned(default))),
        visible_if: None,
    }
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub struct TimelineKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> TimelineKind {
    TimelineKind {
        descriptor: NodeDescriptor {
            id: ID.into(),
            label: "Timeline".into(),
            category: Category::Action,
            inputs: vec![
                Port { name: INPUT.into(), port_type: PortType { modality: Modality::Video, many: true } },
                Port { name: AUDIO_INPUT.into(), port_type: PortType { modality: Modality::Audio, many: true } },
                Port { name: STILLS_INPUT.into(), port_type: PortType { modality: Modality::Image, many: true } },
            ],
            outputs: vec![Port { name: OUTPUT.into(), port_type: PortType { modality: Modality::Video, many: false } }],
            params: ParamSchema {
                params: vec![
                    edge("width", "Width", DEFAULT_WIDTH),
                    edge("height", "Height", DEFAULT_HEIGHT),
                    ParamSpec {
                        name: "fps".into(),
                        label: "Frame rate".into(),
                        description: Some("Frames per second.".into()),
                        control: ParamControl::Choice { options: RATES.iter().map(|r| Choice { value: (*r).into(), label: (*r).into() }).collect() },
                        required: true,
                        default: Some(Value::Text("30".into())),
                        visible_if: None,
                    },
                    ParamSpec {
                        name: "quality".into(),
                        label: "Quality".into(),
                        description: Some("Draft makes the smallest file, with the least detail; high keeps more detail in a larger file.".into()),
                        control: ParamControl::Choice { options: QUALITIES.iter().map(|q| Choice { value: (*q).into(), label: capitalised(q) }).collect() },
                        required: true,
                        default: Some(Value::Text("standard".into())),
                        visible_if: None,
                    },
                    ParamSpec {
                        name: "background".into(),
                        label: "Background".into(),
                        description: Some("Shown around a video that doesn't fill the frame.".into()),
                        control: ParamControl::Color,
                        required: true,
                        default: Some(Value::Text("#000000".into())),
                        visible_if: None,
                    },
                ],
            },
            quick_add: None,
        },
    }
}

impl NodeKind for TimelineKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    /// A card made before the sound and picture inputs (#576) has only its clips port, the
    /// first of today's: it keeps running with that.
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        if node.inputs.is_empty() || node.inputs.len() >= self.descriptor.inputs.len() || !self.descriptor.inputs.starts_with(&node.inputs) {
            return Ok(Cow::Borrowed(&self.descriptor));
        }
        let mut descriptor = self.descriptor.clone();
        descriptor.inputs.truncate(node.inputs.len());
        Ok(Cow::Owned(descriptor))
    }
    fn migrate(&self, node: &Node) -> Result<Option<NodeMigration>, Error> {
        if node.inputs == self.descriptor.inputs || !self.descriptor.inputs.starts_with(&node.inputs) {
            return Ok(None);
        }
        let mut node = node.clone();
        node.inputs = self.descriptor.inputs.clone();
        Ok(Some(NodeMigration { node, warnings: Vec::new() }))
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.iter().all(|t| matches!(t.modality, Modality::Video | Modality::Audio | Modality::Image)).then(|| ModeId::new("action").ok()).flatten()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut params = self.validate_instance(ctx.node, true)?.into_visible_values();
        params.insert(SPEC_PARAM.into(), Value::Text(stored(ctx.node)?.into()));
        let mut inputs = Vec::new();
        let mut sources = Vec::new();
        let mut ports = Vec::new();
        for input in ctx.inputs.iter().filter(|i| Input::from_port(&i.port).is_some()) {
            let values = match &input.value {
                Value::List(values) => values.clone(),
                single => vec![single.clone()],
            };
            for (index, value) in values.into_iter().enumerate() {
                let origin = input.origins.get(index).map(ToString::to_string).unwrap_or_default();
                sources.push(Value::Text(origin));
                ports.push(Value::Text(input.port.clone()));
                inputs.push(value);
            }
        }
        if inputs.is_empty() {
            return Err(invalid("connect a video, sound or picture to the timeline first"));
        }
        params.insert(SOURCES_PARAM.into(), Value::List(sources));
        params.insert(PORTS_PARAM.into(), Value::List(ports));
        Ok(vec![JobSpec::Action { node: ctx.node.id, action: ID.into(), params, inputs, output: OUTPUT.into() }])
    }
}

pub mod interchange;

#[cfg(test)]
mod tests;
