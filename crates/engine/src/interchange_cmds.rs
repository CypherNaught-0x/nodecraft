//! Timeline interchange (#166): a Timeline node's clips as an FCPXML document another editor can
//! open, with the media they play. `timeline.interchange` shows the document and what it leaves
//! out; `timeline.export_interchange` writes it as one archive, `<name>.fcpxml`, `report.txt`
//! and `media/`, holding each source the clips play once. Neither renders nor runs anything:
//! the clips are the node's saved ones and the media each wired node's active result, and a wire
//! with no result yet is an error rather than a hole. See `docs/timeline-interchange.md`.
use crate::commands::{CommandSpec, Context, Effect, params};
use crate::timeline_cmds::{editor, inputs, stored};
use nodecraft_format::interchange::Bundle;
use nodecraft_nodes::timeline::interchange::{self, Interchange, MediaKind, Sequence, Source};
use nodecraft_nodes::timeline::{ID, Wire};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Longest file name stem, in characters.
const MAX_STEM_CHARS: usize = 48;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewParams {
    canvas: CanvasId,
    id: NodeId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportParams {
    canvas: CanvasId,
    id: NodeId,
    path: PathBuf,
    #[serde(default)]
    overwrite: bool,
}

/// `name` as a file name stem: letters, digits, `-` and `_`, every other run of characters one
/// `-`, at most [`MAX_STEM_CHARS`]; `fallback` when nothing is left.
fn stem(name: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if out.chars().count() >= MAX_STEM_CHARS {
            break;
        }
        if c.is_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() { fallback.into() } else { trimmed.into() }
}

/// The file extension for `mime`.
fn extension(mime: &str) -> String {
    let known = match mime {
        "image/jpeg" => Some("jpg"),
        "image/svg+xml" => Some("svg"),
        "video/quicktime" => Some("mov"),
        "video/x-matroska" => Some("mkv"),
        "audio/mpeg" => Some("mp3"),
        "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Some("wav"),
        _ => None,
    };
    if let Some(ext) = known {
        return ext.into();
    }
    let subtype = mime.rsplit('/').next().unwrap_or_default().split(['+', ';', '.']).next().unwrap_or_default();
    let subtype = subtype.strip_prefix("x-").unwrap_or(subtype);
    if !subtype.is_empty() && subtype.len() <= 8 && subtype.chars().all(|c| c.is_ascii_alphanumeric()) { subtype.to_ascii_lowercase() } else { "bin".into() }
}

/// A Timeline's document and the assets it names, in the order of its sources.
struct Prepared {
    name: String,
    fps: u32,
    interchange: Interchange,
    sources: Vec<(Source, AssetRef)>,
}

fn prepare(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Prepared, Error> {
    let node = editor(c, canvas, id)?;
    let timeline = stored(node)?;
    let values = c.kinds.get(ID)?.validate_instance(node, true)?.into_visible_values();
    let frame = crate::action_exec::timeline::frame(&values).map_err(|message| Error::new(ErrorKind::InvalidInput, message))?;
    let wired = inputs(c, canvas, id)?;
    let titles: BTreeMap<String, &str> =
        c.document.canvases().get(&canvas).map(|canvas| canvas.nodes().values().map(|n| (n.id.to_string(), n.title.as_str())).collect()).unwrap_or_default();
    // Each asset once, numbered in the order it is first wired; a picture and a video are
    // never the same bytes, but the kind is part of the key so a source has one kind. The
    // archive's media limit applies to what the document uses, when the bundle is checked.
    let mut sources: Vec<(Source, AssetRef)> = Vec::new();
    let mut seen: BTreeMap<(nodecraft_types::Blake3, u8), usize> = BTreeMap::new();
    let mut media = Vec::with_capacity(wired.len());
    let mut wires: Vec<Wire> = Vec::with_capacity(wired.len());
    for (wire, asset) in wired {
        let kind = interchange::kind_of(wire.input);
        let index = match asset {
            None => None,
            Some(asset) => {
                let key = (asset.hash, kind as u8);
                Some(match seen.get(&key) {
                    Some(index) => *index,
                    None => {
                        let title = titles.get(&wire.origin).copied().unwrap_or_default();
                        let name = if title.trim().is_empty() { format!("Source {}", sources.len().saturating_add(1)) } else { title.trim().to_owned() };
                        let file = format!("media/{:02}-{}.{}", sources.len().saturating_add(1), stem(&name, "source"), extension(asset.mime.essence()));
                        let sound = match kind {
                            MediaKind::Audio => true,
                            MediaKind::Video => asset.meta.channels.is_some_and(|channels| channels > 0),
                            MediaKind::Still => false,
                        };
                        let duration_us = match kind {
                            MediaKind::Still => None,
                            _ => asset.meta.duration_ms.and_then(|ms| ms.checked_mul(1000)),
                        };
                        seen.insert(key, sources.len());
                        sources.push((Source { name, file, kind, sound, duration_us }, asset));
                        sources.len().saturating_sub(1)
                    }
                })
            }
        };
        media.push(index);
        wires.push(wire);
    }
    let name = if node.title.trim().is_empty() { "Timeline".to_owned() } else { node.title.trim().to_owned() };
    let listed: Vec<Source> = sources.iter().map(|(source, _)| source.clone()).collect();
    let sequence = Sequence {
        name: &name,
        width: frame.width,
        height: frame.height,
        fps: frame.fps.0,
        background: frame.background,
        timeline: &timeline,
        wires: &wires,
        media: &media,
        sources: &listed,
    };
    let interchange = interchange::fcpxml(&sequence)?;
    for index in &interchange.used {
        let (_, asset) = sources.get(*index).ok_or_else(|| Error::new(ErrorKind::Internal, "the document names media that isn't listed"))?;
        c.validate_asset(asset)?;
    }
    Ok(Prepared { name, fps: frame.fps.0, interchange, sources })
}

/// What a person reads beside the document: what it is, and everything it leaves out.
fn report(prepared: &Prepared) -> String {
    let i = &prepared.interchange;
    let mut out = format!(
        "NodeCraft timeline interchange\n\nDocument: {}.fcpxml (FCPXML {})\nSequence: {}, {} frames at {} fps\nMedia: {} file(s) in media/\n\n",
        stem(&prepared.name, "Timeline"),
        interchange::VERSION,
        prepared.name,
        i.frames,
        prepared.fps,
        i.used.len()
    );
    if i.srt.is_some() {
        out.push_str(&format!("Subtitles: {}.srt (UTF-8 SubRip text and timing; no style or position)\n\n", stem(&prepared.name, "Timeline")));
    }
    if i.losses.is_empty() {
        out.push_str("No losses detected.\n");
    } else {
        out.push_str("Left out or changed:\n");
        for loss in &i.losses {
            match (&loss.clip, &loss.caption) {
                (_, Some(caption)) => out.push_str(&format!("- caption {caption}: {}: {}\n", loss.feature, loss.detail)),
                (Some(clip), _) => out.push_str(&format!("- clip {clip}: {}: {}\n", loss.feature, loss.detail)),
                _ => out.push_str(&format!("- {}: {}\n", loss.feature, loss.detail)),
            }
        }
    }
    out.push_str("\nImporting this document into other editors hasn't been verified yet; see NodeCraft's docs/timeline-interchange.md.\n");
    out
}

fn bundle(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<(Prepared, Bundle), Error> {
    let prepared = prepare(c, canvas, id)?;
    let report = report(&prepared);
    let mut media = Vec::with_capacity(prepared.interchange.used.len());
    for index in &prepared.interchange.used {
        let (source, asset) = prepared.sources.get(*index).ok_or_else(|| Error::new(ErrorKind::Internal, "the document names media that isn't listed"))?;
        media.push((source.file.clone(), asset.clone()));
    }
    let subtitles = prepared.interchange.srt.as_ref().map(|srt| (format!("{}.srt", stem(&prepared.name, "Timeline")), srt.clone()));
    let bundle = Bundle { document: format!("{}.fcpxml", stem(&prepared.name, "Timeline")), xml: prepared.interchange.xml.clone(), report, subtitles, media };
    bundle.check()?;
    Ok((prepared, bundle))
}

fn preview(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PreviewParams = params(value)?;
    let (prepared, bundle) = bundle(c, p.canvas, p.id)?;
    let media: Vec<Json> = prepared
        .interchange
        .used
        .iter()
        .filter_map(|index| prepared.sources.get(*index))
        .map(|(source, asset)| {
            let kind = match source.kind {
                MediaKind::Video => "video",
                MediaKind::Audio => "audio",
                MediaKind::Still => "still",
            };
            json!({"file": source.file, "name": source.name, "kind": kind, "asset": asset})
        })
        .collect();
    Ok(Effect::None {
        result: json!({
            "format": interchange::FORMAT,
            "version": interchange::VERSION,
            "document": bundle.document,
            "frames": prepared.interchange.frames,
            "media": media,
            "losses": prepared.interchange.losses,
            "xml": bundle.xml,
            "subtitles": bundle.subtitles.as_ref().map(|(file, text)| json!({"file": file, "format": "srt", "text": text})),
            "report": bundle.report,
        }),
    })
}

fn export(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExportParams = params(value)?;
    crate::export::check(&p.path)?;
    let (_, bundle) = bundle(c, p.canvas, p.id)?;
    Ok(Effect::Export { what: crate::export::Exported::Bundle(Box::new(bundle)), path: p.path, overwrite: p.overwrite })
}

fn preview_enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    preview(c, value).map(|_| ())
}

fn export_enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    export(c, value).map(|_| ())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn preview_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id"], "properties": {"canvas": uuid(), "id": uuid()}})
}

fn export_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "path"],
        "properties": {"canvas": uuid(), "id": uuid(),
            "path": {"type": "string", "minLength": 1, "maxLength": crate::export::MAX_PATH_BYTES},
            "overwrite": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "timeline.interchange",
            label: "Preview interchange export",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: preview_schema,
            params_doc: "{canvas, id: Timeline NodeId} -> {format: \"fcpxml\", version, document, frames, media: [{file, name, kind: video|audio|still, asset}], losses: [{clip?, caption?, feature, detail}], xml, subtitles: {file, format: \"srt\", text}|null, report}; the FCPXML document with caption titles and optional SRT companion timeline.export_interchange would write, and every setting left out or changed. Writes nothing. Fails, naming the clip, when wired media is missing or a clip doesn't fit it.",
            enabled: preview_enabled,
            run: preview,
        },
        CommandSpec {
            id: "timeline.export_interchange",
            label: "Export for another editor…",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: export_schema,
            params_doc: "{canvas, id: Timeline NodeId, path: absolute file path, overwrite?: bool (default false)} -> {bytes}; writes a ZIP holding <name>.fcpxml with caption titles, optional <name>.srt (caption text/timing), report.txt (every loss) and media/ with each source once at relative paths. Nothing is rendered and no provider is called. Refuses an existing file unless `overwrite`, a missing folder and a full disk, leaving nothing half written. Desktop only.",
            enabled: export_enabled,
            run: export,
        },
    ]
}

#[cfg(test)]
mod tests;
