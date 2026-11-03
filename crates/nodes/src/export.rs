//! `media.export` (#746): a sink card that names how the media wired into it is written to a
//! folder. Its ports and parameters are data, so the card, inspector and MCP schema come from them.
//!
//! The node never runs. The run planner resolves it away like a router (see
//! `docs/decisions/2026-10-28-media-export-node.md`): writing files is a host effect performed by
//! the explicit `export.node_plan` / `export.node_run` commands, through a folder a person granted
//! in the desktop app. That grant is view state, never a parameter: the only destination
//! parameter here is a `subfolder` *inside* it.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, action::port};
use nodecraft_graph::Node;
use nodecraft_types::{Choice, Error, ErrorKind, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Value};

/// The kind id.
pub const ID: &str = "media.export";
/// The filename pattern parameter.
pub const TEMPLATE: &str = "template";
/// The collision policy parameter.
pub const ON_COLLISION: &str = "on_collision";
/// The output format parameter.
pub const FORMAT: &str = "format";
/// The relative folder inside the granted folder.
pub const SUBFOLDER: &str = "subfolder";
/// The default filename pattern: the batch export's own default.
pub const DEFAULT_TEMPLATE: &str = "{title}-{output}-{index}";
/// Longest pattern the batch export accepts.
pub const MAX_TEMPLATE_BYTES: usize = 128;
/// Longest relative subfolder.
pub const MAX_SUBFOLDER_BYTES: usize = 512;
/// The media input ports, in the order the export lists their sources.
pub const INPUTS: [(&str, Modality); 5] =
    [("image", Modality::Image), ("video", Modality::Video), ("audio", Modality::Audio), ("mesh", Modality::Mesh), ("document", Modality::Document)];

fn choice(value: &str, label: &str) -> Choice {
    Choice { value: value.into(), label: label.into() }
}

fn descriptor() -> NodeDescriptor {
    NodeDescriptor {
        id: ID.into(),
        label: "Export to folder".into(),
        category: Category::Structure,
        inputs: INPUTS.iter().map(|(name, modality)| port(name, *modality, true)).collect(),
        outputs: Vec::new(),
        quick_add: None,
        params: ParamSchema {
            params: vec![
                ParamSpec {
                    name: TEMPLATE.into(),
                    label: "File names".into(),
                    description: Some(
                        "A file-name pattern; the stored extension is added. Tokens: {index}, {node}, {generation}, {kind}, {title}, {output}, {item}, {hash}."
                            .into(),
                    ),
                    control: ParamControl::Text { multiline: false, max_bytes: MAX_TEMPLATE_BYTES },
                    required: true,
                    default: Some(Value::Text(DEFAULT_TEMPLATE.into())),
                    visible_if: None,
                },
                ParamSpec {
                    name: ON_COLLISION.into(),
                    label: "If a file exists".into(),
                    description: Some("Keep leaves an existing file alone and reports it; Number writes name (2); Replace swaps it atomically.".into()),
                    control: ParamControl::Choice {
                        options: vec![choice("skip", "Keep the existing file"), choice("numbered", "Number the new file"), choice("overwrite", "Replace it")],
                    },
                    required: true,
                    default: Some(Value::Text("skip".into())),
                    visible_if: None,
                },
                ParamSpec {
                    name: FORMAT.into(),
                    label: "Format".into(),
                    description: Some("Writes the stored bytes unchanged; conversion is not available in this build.".into()),
                    control: ParamControl::Choice { options: vec![choice("original", "Original file")] },
                    required: true,
                    default: Some(Value::Text("original".into())),
                    visible_if: None,
                },
                ParamSpec {
                    name: SUBFOLDER.into(),
                    label: "Subfolder".into(),
                    description: Some(
                        "Optional folder inside the folder you chose, which must already exist. The chosen folder itself is never saved in the project.".into(),
                    ),
                    control: ParamControl::Text { multiline: false, max_bytes: MAX_SUBFOLDER_BYTES },
                    required: false,
                    default: Some(Value::Text(String::new())),
                    visible_if: None,
                },
            ],
        },
    }
}

/// What a `media.export` node asks for, read from its validated parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub template: String,
    /// `skip`, `numbered` or `overwrite`.
    pub on_collision: String,
    /// `original`.
    pub format: String,
    /// Relative path components inside the granted folder; empty for the folder itself.
    pub subfolder: Vec<String>,
}

fn text<'a>(values: &'a nodecraft_types::ParamValues, name: &str) -> Option<&'a str> {
    match values.get(name) {
        Some(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// Splits a subfolder into plain components. `/` and `\` both separate; empty and `.` parts are
/// dropped. `..`, a drive or root prefix, control characters and over-long input are refused, so
/// the result can only name a folder *inside* the grant.
pub fn subfolder_components(value: &str) -> Result<Vec<String>, Error> {
    if value.len() > MAX_SUBFOLDER_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "the subfolder is longer than 512 bytes"));
    }
    if value.chars().any(char::is_control) || value.contains(':') {
        return Err(Error::new(ErrorKind::InvalidInput, "the subfolder must be a plain relative folder name, such as renders/final"));
    }
    if value.starts_with(['/', '\\']) {
        return Err(Error::new(ErrorKind::InvalidInput, "the subfolder must be relative to the folder you chose, not an absolute path"));
    }
    let mut parts = Vec::new();
    for part in value.split(['/', '\\']) {
        if part.trim() != part {
            return Err(Error::new(ErrorKind::InvalidInput, "a subfolder name cannot start or end with a space; remove the spaces"));
        }
        match part {
            "" | "." => {}
            ".." => return Err(Error::new(ErrorKind::InvalidInput, "the subfolder cannot contain '..'; it must stay inside the folder you chose")),
            name => parts.push(name.to_owned()),
        }
    }
    Ok(parts)
}

/// The export settings of a stored `media.export` node, validated against this kind's schema.
pub fn settings(kind: &dyn NodeKind, node: &Node) -> Result<Settings, Error> {
    if node.kind != ID {
        return Err(Error::new(ErrorKind::InvalidInput, "this is not an Export to folder card"));
    }
    let values = kind.validate_instance(node, true)?.values;
    let template = text(&values, TEMPLATE).unwrap_or(DEFAULT_TEMPLATE).to_owned();
    let on_collision = text(&values, ON_COLLISION).unwrap_or("skip").to_owned();
    let format = text(&values, FORMAT).unwrap_or("original").to_owned();
    let subfolder = subfolder_components(text(&values, SUBFOLDER).unwrap_or_default())?;
    Ok(Settings { template, on_collision, format, subfolder })
}

pub struct ExportKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> ExportKind {
    ExportKind { descriptor: descriptor() }
}

impl NodeKind for ExportKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    /// Any mix of media wires (or none yet) is an export; text and styles never are, since
    /// they are copied, not written to files.
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.iter().all(|port| INPUTS.iter().any(|(_, modality)| *modality == port.modality)).then(|| ModeId::new("export").ok()).flatten()
    }
    /// Never a job: the planner resolves the node away, and writing files is the explicit
    /// `export.node_run` command's host effect. Validating keeps a malformed card reported.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        settings(self, ctx.node)?;
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests;
