//! The Timeline kind (#109): every video wired into `clips` becomes a clip on one track, played
//! in order, each trimmed, faded, crossed into the next and mixed at its own volume; the output
//! is one rendered video. The clips live in the node's `config["nodecraft.timeline"]` as JSON
//! (`{"clips": [...]}`, first first; the format is [`Timeline`]). The host checks them where they
//! are set (`timeline.*` commands) and again, against each video's length, before it renders;
//! this kind only carries them, with the node each connected video came from, so a clip stays
//! with its video when wires are reordered.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Choice, Error, ErrorKind, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Unsigned, Value};
use serde::{Deserialize, Serialize};

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
/// The input port.
pub const INPUT: &str = "clips";
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
pub const MAX_ENCODED_BYTES: usize = 64 * 1024;
/// Longest time anywhere on a timeline: an hour, in microseconds.
pub const MAX_US: u64 = 3_600_000_000;
/// Shortest cross transition; shorter ones are a cut (0).
pub const MIN_TRANSITION_US: u64 = 10_000;
/// Loudest clip volume, in thousandths (4000 is four times as loud).
pub const MAX_VOLUME: u16 = 4000;
/// Longest clip id, in bytes.
pub const MAX_ID_BYTES: usize = 64;

/// The clips of one track, first first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub clips: Vec<Clip>,
}

/// One clip: a stretch of the `nth` video from node `origin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: String,
    /// The id of the node the video comes from.
    pub origin: String,
    /// Which of that node's videos, from 0.
    #[serde(default)]
    pub nth: u32,
    /// Where in the video the clip starts, in microseconds.
    #[serde(default)]
    pub in_us: u64,
    /// Where it ends; the end of the video when unset.
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
}

fn unity() -> u16 {
    1000
}

fn yes() -> bool {
    true
}

impl Clip {
    /// A whole video, as wired in: no trim, fade or transition.
    pub fn whole(id: String, origin: String, nth: u32) -> Self {
        Self { id, origin, nth, in_us: 0, out_us: None, fade_in_us: 0, fade_out_us: 0, volume: unity(), muted: false, transition_us: 0, enabled: true }
    }

    /// The clip's length, when its end is set.
    pub fn length_us(&self) -> Option<u64> {
        self.out_us.map(|out| out.saturating_sub(self.in_us))
    }

    pub fn check(&self) -> Result<(), Error> {
        if !valid_id(&self.id) {
            return Err(bad("a clip id is 1 to 64 letters, digits, - or _"));
        }
        if !valid_id(&self.origin) {
            return Err(bad("a clip's origin is the id of the node its video comes from"));
        }
        let times = [self.in_us, self.out_us.unwrap_or(0), self.fade_in_us, self.fade_out_us, self.transition_us];
        if times.iter().any(|t| *t > MAX_US) {
            return Err(bad("a clip's times are within an hour"));
        }
        if self.out_us.is_some_and(|out| out <= self.in_us) {
            return Err(bad("a clip ends after it starts"));
        }
        if self.length_us().is_some_and(|length| self.fade_in_us.saturating_add(self.fade_out_us) > length) {
            return Err(bad("a clip's fades are no longer than the clip"));
        }
        if self.transition_us != 0 && self.transition_us < MIN_TRANSITION_US {
            return Err(bad("a transition is 0 (a cut) or at least 10 ms"));
        }
        if self.volume > MAX_VOLUME {
            return Err(bad("a clip's volume is 0 to 4000 thousandths"));
        }
        Ok(())
    }
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
        let mut ids = std::collections::BTreeSet::new();
        for clip in &self.clips {
            clip.check()?;
            if !ids.insert(clip.id.as_str()) {
                return Err(Error::new(ErrorKind::Conflict, "two clips have the same id"));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Every clip the node shows, given the node each connected video came from (`sources`, in
    /// input order; an unknown origin is empty): the stored ones, then a whole-video clip for
    /// each connected video no stored clip names, in input order.
    pub fn with_sources(&self, sources: &[String]) -> Self {
        let keys = source_keys(sources);
        let mut taken: std::collections::BTreeSet<String> = self.clips.iter().map(|c| c.id.clone()).collect();
        let mut clips = self.clips.clone();
        for (origin, nth) in keys {
            if self.names(&origin, nth) {
                continue;
            }
            let base = auto_id(&origin, nth);
            let mut id = base.clone();
            let mut n = 2u32;
            while taken.contains(&id) {
                id = format!("{}-{n}", base.get(..base.len().min(MAX_ID_BYTES.saturating_sub(12))).unwrap_or("clip"));
                n = n.saturating_add(1);
            }
            taken.insert(id.clone());
            clips.push(Clip::whole(id, origin, nth));
        }
        Self { clips }
    }

    /// The clips to render, each with the input it plays: the enabled clips whose video is
    /// connected, in order. The last plays out with no transition, since nothing follows it.
    pub fn resolve(&self, sources: &[String]) -> Vec<(usize, Clip)> {
        let keys = source_keys(sources);
        let mut out: Vec<(usize, Clip)> = self
            .with_sources(sources)
            .clips
            .into_iter()
            .filter(|c| c.enabled)
            .filter_map(|c| keys.iter().position(|(o, n)| *o == c.origin && *n == c.nth).map(|input| (input, c)))
            .collect();
        if let Some((_, last)) = out.last_mut() {
            last.transition_us = 0;
        }
        out
    }

    /// Whether a stored clip plays the `nth` video from `origin`.
    fn names(&self, origin: &str, nth: u32) -> bool {
        self.clips.iter().any(|c| c.origin == origin && c.nth == nth)
    }

    /// Whether `clip`'s video is connected now.
    pub fn is_wired(clip: &Clip, sources: &[String]) -> bool {
        source_keys(sources).iter().any(|(o, n)| *o == clip.origin && *n == clip.nth)
    }
}

/// Each source's (origin, nth video from that origin). Unknown origins count as distinct.
fn source_keys(sources: &[String]) -> Vec<(String, u32)> {
    let mut seen: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    sources
        .iter()
        .enumerate()
        .map(|(input, origin)| {
            if !valid_id(origin) {
                return (format!("input{input}"), 0);
            }
            let nth = seen.entry(origin.as_str()).or_insert(0);
            let key = (origin.clone(), *nth);
            *nth = nth.saturating_add(1);
            key
        })
        .collect()
}

/// The clip id a video from `origin` gets until it is stored: stable while that wire stays.
pub fn auto_id(origin: &str, nth: u32) -> String {
    let id = format!("{origin}-{nth}");
    if valid_id(&id) { id } else { format!("clip-{nth}") }
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

pub struct TimelineKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> TimelineKind {
    TimelineKind {
        descriptor: NodeDescriptor {
            id: ID.into(),
            label: "Timeline".into(),
            category: Category::Action,
            inputs: vec![Port { name: INPUT.into(), port_type: PortType { modality: Modality::Video, many: true } }],
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
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.iter().all(|t| t.modality == Modality::Video).then(|| ModeId::new("action").ok()).flatten()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut params = self.validate_instance(ctx.node, true)?.into_visible_values();
        params.insert(SPEC_PARAM.into(), Value::Text(stored(ctx.node)?.into()));
        let mut inputs = Vec::new();
        let mut sources = Vec::new();
        for input in ctx.inputs.iter().filter(|i| i.port == INPUT) {
            let values = match &input.value {
                Value::List(values) => values.clone(),
                single => vec![single.clone()],
            };
            for (index, value) in values.into_iter().enumerate() {
                let origin = input.origins.get(index).map(ToString::to_string).unwrap_or_default();
                sources.push(Value::Text(origin));
                inputs.push(value);
            }
        }
        if inputs.is_empty() {
            return Err(invalid("connect a video to the timeline first"));
        }
        params.insert(SOURCES_PARAM.into(), Value::List(sources));
        Ok(vec![JobSpec::Action { node: ctx.node.id, action: ID.into(), params, inputs, output: OUTPUT.into() }])
    }
}

#[cfg(test)]
mod tests;
