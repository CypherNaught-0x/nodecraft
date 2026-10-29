//! Original FCPXML adjustment envelopes. Keyframe times are in the clip's source clock.
//! Audio gain is sampled on the sequence frame grid before conversion to dB; a linear
//! dB ramp from silence to the peak would otherwise make most of a fade nearly inaudible.
use super::{Clip, Error, MediaKind, SILENT_DB, UNIT, decibels, frames, invalid, time};
use std::fmt::Write as _;

pub(super) const MAX_KEYFRAMES: usize = 65_536;
pub(super) struct Budget<'a> {
    remaining: usize,
    check: &'a mut dyn FnMut() -> Result<(), Error>,
}
impl<'a> Budget<'a> {
    pub(super) fn new(check: &'a mut dyn FnMut() -> Result<(), Error>) -> Self {
        Self { remaining: MAX_KEYFRAMES, check }
    }
    pub(super) fn check(&mut self) -> Result<(), Error> {
        (self.check)()
    }
    fn take(&mut self, n: usize) -> Result<(), Error> {
        (self.check)()?;
        self.remaining = self.remaining.checked_sub(n).ok_or_else(too_many)?;
        Ok(())
    }
}
fn too_many() -> Error {
    invalid("the timeline has too many fade keyframes; shorten fades or lower the frame rate".into())
}

#[derive(Clone, Debug)]
pub(super) struct Envelope {
    origin: u64,
    length: u64,
    fade_in: u64,
    fade_out: u64,
    opacity: f64,
    volume: Option<f64>,
    picture: bool,
    audio: bool,
}
impl Envelope {
    /// NodeCraft fades use played time. The interchange currently retains unretimed media,
    /// so convert the fade positions to source time too; the existing speed loss remains.
    pub(super) fn new(clip: &Clip, kind: MediaKind, sound: bool, fps: u32, origin: u64, length: u64) -> Result<(Self, bool), Error> {
        if fps == 0 || clip.speed == 0 || clip.opacity > UNIT {
            return Err(invalid("a fade needs a positive frame/playback rate and opacity from 0 to 1000".into()));
        }
        let source_frames = |us: u64| -> Result<(u64, bool), Error> {
            let scaled = u128::from(us).checked_mul(u128::from(clip.speed)).ok_or_else(too_many)?;
            let source_us = u64::try_from(scaled / u128::from(UNIT)).map_err(|_| too_many())?;
            let (n, exact) = frames(source_us, fps);
            Ok((n, exact && scaled % u128::from(UNIT) == 0))
        };
        let (fade_in, in_exact) = source_frames(clip.fade_in_us)?;
        let (fade_out, out_exact) = source_frames(clip.fade_out_us)?;
        let in_f = fade_in.min(length);
        let out_f = fade_out.min(length.saturating_sub(in_f));
        let audio = kind == MediaKind::Audio || (kind == MediaKind::Video && sound);
        Ok((
            Self {
                origin,
                length,
                fade_in: in_f,
                fade_out: out_f,
                opacity: f64::from(clip.opacity) / f64::from(UNIT),
                volume: audio.then(|| decibels(clip.volume, clip.muted).unwrap_or(0.0)),
                picture: kind != MediaKind::Audio,
                audio,
            },
            in_exact && out_exact && in_f == fade_in && out_f == fade_out,
        ))
    }
    fn faded(&self) -> bool {
        self.fade_in != 0 || self.fade_out != 0
    }
    pub(super) fn audio_sampled(&self) -> bool {
        self.audio && self.faded() && self.volume.is_some_and(|volume| volume > SILENT_DB)
    }
    pub(super) fn needed(&self) -> bool {
        self.faded() || (self.picture && self.opacity != 1.0) || self.volume.is_some_and(|v| v != 0.0)
    }
    fn gain(&self, at: u64) -> f64 {
        let local = at.saturating_sub(self.origin).min(self.length);
        let enter = if self.fade_in == 0 { 1.0 } else { local as f64 / self.fade_in as f64 };
        let leave = if self.fade_out == 0 { 1.0 } else { self.length.saturating_sub(local) as f64 / self.fade_out as f64 };
        enter.min(leave).clamp(0.0, 1.0)
    }
    fn points(&self, start: u64, length: u64, audio: bool, budget: &mut Budget<'_>) -> Result<Vec<u64>, Error> {
        let end = start.checked_add(length).ok_or_else(too_many)?;
        let own_end = self.origin.checked_add(self.length).ok_or_else(too_many)?;
        let enter_end = self.origin.checked_add(self.fade_in).ok_or_else(too_many)?;
        let leave_start = own_end.checked_sub(self.fade_out).ok_or_else(too_many)?;
        let ranges = [(self.origin.max(start), enter_end.min(end), self.fade_in), (leave_start.max(start), own_end.min(end), self.fade_out)];
        let mut count = 4usize;
        if audio {
            for (a, b, fade) in ranges {
                if fade != 0 && a <= b {
                    let n = b.checked_sub(a).and_then(|n| n.checked_add(1)).and_then(|n| usize::try_from(n).ok()).ok_or_else(too_many)?;
                    count = count.checked_add(n).filter(|n| *n <= MAX_KEYFRAMES).ok_or_else(too_many)?;
                }
            }
        }
        let mut points = Vec::new();
        points.try_reserve_exact(count).map_err(|_| too_many())?;
        points.extend([start, end]);
        for at in [enter_end, leave_start] {
            if at > start && at < end {
                points.push(at);
            }
        }
        if audio {
            for (a, b, fade) in ranges {
                if fade != 0 && a <= b {
                    for at in a..=b {
                        (budget.check)()?;
                        points.push(at);
                    }
                }
            }
        }
        points.sort_unstable();
        points.dedup();
        budget.take(points.len())?;
        Ok(points)
    }
    pub(super) fn write(&self, xml: &mut String, start: u64, length: u64, fps: u32, indent: &str, budget: &mut Budget<'_>) -> Result<(), Error> {
        if self.picture && (self.faded() || self.opacity != 1.0) {
            if self.faded() {
                self.animation(xml, (start, length), fps, indent, false, budget)?;
            } else {
                let _ = writeln!(xml, "{indent}<adjust-blend amount=\"{:.6}\"/>", self.opacity);
            }
        }
        if self.audio {
            if self.faded() {
                self.animation(xml, (start, length), fps, indent, true, budget)?;
            } else if let Some(db) = self.volume.filter(|db| *db != 0.0) {
                let _ = writeln!(xml, "{indent}<adjust-volume amount=\"{db:.2}dB\"/>");
            }
        }
        Ok(())
    }
    fn animation(&self, xml: &mut String, range: (u64, u64), fps: u32, indent: &str, audio: bool, budget: &mut Budget<'_>) -> Result<(), Error> {
        let (start, length) = range;
        let points = self.points(start, length, audio, budget)?;
        if indent.len() > 64 {
            return Err(too_many());
        }
        let capacity = points.len().checked_mul(256).and_then(|n| n.checked_add(1024)).ok_or_else(too_many)?;
        xml.try_reserve(capacity).map_err(|_| too_many())?;
        let adjustment = if audio { "adjust-volume" } else { "adjust-blend" };
        let peak = if audio { format!("{:.2}dB", self.volume.unwrap_or(0.0)) } else { format!("{:.6}", self.opacity) };
        let _ = writeln!(xml, "{indent}<{adjustment} amount=\"{peak}\">\n{indent}  <param name=\"amount\">\n{indent}    <keyframeAnimation>");
        for at in points {
            (budget.check)()?;
            let gain = self.gain(at);
            let value = if audio {
                let db = if gain == 0.0 { SILENT_DB } else { (self.volume.unwrap_or(0.0) + 20.0 * gain.log10()).max(SILENT_DB) };
                format!("{db:.2}dB")
            } else {
                format!("{:.6}", gain * self.opacity)
            };
            let _ = writeln!(xml, "{indent}      <keyframe time=\"{}\" value=\"{value}\" interp=\"linear\" curve=\"linear\"/>", time(at, fps));
        }
        let _ = writeln!(xml, "{indent}    </keyframeAnimation>\n{indent}  </param>\n{indent}</{adjustment}>");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
