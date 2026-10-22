//! Caption timing and text shared by the FCPXML titles and their SubRip companion (#649).
use super::{Loss, Sequence, Timeline, escape, frames, invalid, time};
use crate::timeline::caption::{Align, Caption, MAX_CAPTIONS, Position};
use nodecraft_types::Error;
use std::fmt::Write as _;

const TITLE_REF: &str = "rCaptionTitle";
// This names the destination editor's built-in template; no Motion template is bundled.
const TITLE_UID: &str = ".../Titles.localized/Bumper:Opener.localized/Basic Title.localized/Basic Title.moti";

pub(super) struct Placed<'a> {
    caption: &'a Caption,
    pub from: u64,
    to: u64,
}

fn loss(losses: &mut Vec<Loss>, caption: &Caption, feature: &'static str, detail: String) {
    losses.push(Loss { clip: None, caption: Some(caption.id.clone()), feature, detail });
}

pub(super) fn prepare<'a>(timeline: &'a Timeline, fps: u32, total: u64, losses: &mut Vec<Loss>) -> Result<Vec<Placed<'a>>, Error> {
    if timeline.captions.len() > MAX_CAPTIONS {
        return Err(invalid("the timeline has too many captions to export".into()));
    }
    let mut placed = Vec::new();
    for caption in &timeline.captions {
        caption.check()?;
        if !caption.enabled {
            loss(losses, caption, "disabled caption", "left out, as a render leaves it out".into());
            continue;
        }
        let (from, start_exact) = frames(caption.start_us, fps);
        let (end, end_exact) = frames(caption.end_us, fps);
        let to = end.max(from.saturating_add(1)).min(total);
        if from >= to {
            loss(losses, caption, "caption outside sequence", "left out: it starts after the exported sequence ends".into());
            continue;
        }
        if !start_exact || !end_exact {
            loss(
                losses,
                caption,
                "caption sub-frame timing",
                format!("start and end rounded to whole frames at {fps} fps, at least one frame, as in the render"),
            );
        }
        if end > total {
            loss(losses, caption, "caption clipped to sequence", "the caption ends with the exported sequence".into());
        }
        loss(
            losses,
            caption,
            "caption style",
            format!(
                "FCPXML uses the destination editor's Basic Title appearance; font {:?}, size {} px, fill {}, outline {} ({} px) are not transferred. SRT carries no style or box.",
                caption.font, caption.size, caption.color, caption.stroke, caption.stroke_width,
            ),
        );
        loss(
            losses,
            caption,
            "caption placement",
            format!(
                "FCPXML requests {:?} horizontal alignment and a {:?} frame anchor; its template's text metrics and exact 5% margin may differ. SRT carries no position or alignment.",
                caption.align, caption.position,
            ),
        );
        if caption.text.chars().any(|c| matches!(c, '\u{FFFE}' | '\u{FFFF}')) {
            loss(losses, caption, "caption XML text", "XML 1.0 noncharacters become spaces in the title; the SRT preserves them".into());
        }
        if caption.text.split('\n').any(|line| line.trim().is_empty()) {
            loss(
                losses,
                caption,
                "caption SRT blank lines",
                "empty text lines contain a zero-width space in SRT so they cannot terminate a cue; FCPXML keeps the original line breaks".into(),
            );
        }
        if caption.text.contains(['&', '<', '>']) {
            loss(
                losses,
                caption,
                "caption SRT literal markup",
                "literal &, < and > are escaped as HTML entities in SRT; readers differ in how they display entities".into(),
            );
        }
        // SubRip is a millisecond grid; retain the rendered frame timing to the nearest ms.
        if from.saturating_mul(1000) % u64::from(fps) != 0 || to.saturating_mul(1000) % u64::from(fps) != 0 {
            loss(losses, caption, "caption SRT timing", "frame times rounded to the nearest millisecond in SRT, with at least one millisecond per cue".into());
        }
        placed.push(Placed { caption, from, to });
    }
    Ok(placed)
}

pub(super) fn resource(xml: &mut String) {
    let _ = writeln!(xml, "    <effect id=\"{TITLE_REF}\" name=\"Basic Title\" uid=\"{}\"/>", escape(TITLE_UID));
}

/// Text node escaping preserves newlines; attribute escaping deliberately does not.
fn text(text: &str) -> String {
    text.split('\n').map(escape).collect::<Vec<_>>().join("\n")
}

fn thousandths(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let value = value.unsigned_abs();
    format!("{sign}{}.{:03}", value / 1000, value % 1000)
}

pub(super) fn title(xml: &mut String, placed: &Placed<'_>, lane: i32, local: u64, sequence: &Sequence<'_>) -> Result<(), Error> {
    if sequence.height == 0 {
        return Err(invalid("the caption frame height is 0".into()));
    }
    let caption = placed.caption;
    let align = match caption.align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    };
    // FCPXML translations are percentages of frame height, positive y up. These are frame
    // anchors, rather than an assertion about the destination template's text bounding box.
    let horizontal = i64::from(sequence.width) * 45_000 / i64::from(sequence.height);
    let x = match caption.align {
        Align::Left => -horizontal,
        Align::Center => 0,
        Align::Right => horizontal,
    };
    let y = match caption.position {
        Position::Top => 45_000,
        Position::Middle => 0,
        Position::Bottom => -45_000,
    };
    let _ = writeln!(
        xml,
        "              <title ref=\"{TITLE_REF}\" lane=\"{lane}\" name=\"{}\" offset=\"{}\" start=\"0s\" duration=\"{}\" role=\"titles\">",
        escape(&caption.id),
        time(local, sequence.fps),
        time(placed.to.saturating_sub(placed.from), sequence.fps),
    );
    let _ = writeln!(xml, "                <text><text-style alignment=\"{align}\">{}</text-style></text>", text(&caption.text));
    let _ = writeln!(xml, "                <adjust-transform position=\"{} {}\"/>", thousandths(x), thousandths(y));
    xml.push_str("              </title>\n");
    Ok(())
}

fn milliseconds(frame: u64, fps: u32) -> u128 {
    u128::from(frame).saturating_mul(1000).saturating_add(u128::from(fps) / 2) / u128::from(fps.max(1))
}

fn timestamp(milliseconds: u128) -> String {
    format!("{:02}:{:02}:{:02},{:03}", milliseconds / 3_600_000, milliseconds / 60_000 % 60, milliseconds / 1000 % 60, milliseconds % 1000)
}

pub(super) fn srt(captions: &[Placed<'_>], fps: u32) -> String {
    let mut captions: Vec<_> = captions.iter().enumerate().collect();
    // Stable order for overlapping cues; FCPXML retains the original stacking order instead.
    captions.sort_by_key(|(index, caption)| (caption.from, *index));
    let mut out = String::new();
    for (index, (_, placed)) in captions.iter().enumerate() {
        let start = milliseconds(placed.from, fps);
        let end = milliseconds(placed.to, fps).max(start.saturating_add(1));
        let _ = writeln!(out, "{}\n{} --> {}", index.saturating_add(1), timestamp(start), timestamp(end));
        for line in placed.caption.text.split('\n') {
            let line = if line.trim().is_empty() { "\u{200b}".to_owned() } else { line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") };
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }
    out
}
