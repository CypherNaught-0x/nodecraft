//! Explicit workflow-result saves. Only a trusted, ephemeral ExportHost grants folder writes.
//! Plans snapshot history references/provenance; writes never run as graph edits or replay.
use crate::{
    commands::{CommandSpec, Context, Effect, params},
    local_export::ExportHost,
};
#[cfg(unix)]
use nodecraft_format::export::MAX_COLLISION_SUFFIX;
use nodecraft_format::{
    AssetStore,
    export::{ExportName, MAX_ENTRIES, NameFields, Template},
};
use nodecraft_graph::{Generation, JobSettings, Node};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, GenerationId, NodeId, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{collections::BTreeSet, io::Write, path::PathBuf};

pub const DEFAULT_TEMPLATE: &str = "{title}-{generation}-{index}";
const MAX_MANIFEST: usize = 4 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum History {
    #[default]
    All,
    Active,
}
/// One immutable historical output item; selecting it never changes active history.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSelector {
    pub node: NodeId,
    pub generation: GenerationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    canvas: CanvasId,
    #[serde(default)]
    nodes: Vec<NodeId>,
    #[serde(default)]
    history: History,
    #[serde(default)]
    targets: Option<Vec<ResultSelector>>,
    destination: String,
    #[serde(default)]
    template: Option<String>,
}
#[doc(hidden)]
#[cfg_attr(not(unix), allow(dead_code))]
pub struct Plan {
    host: ExportHost,
    destination: PathBuf,
    entries: Vec<Entry>,
    scope: History,
}
#[cfg_attr(not(unix), allow(dead_code))]
struct Entry {
    name: Option<ExportName>,
    asset: Option<AssetRef>,
    source: Source,
    provenance: Json,
    error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Source {
    node: NodeId,
    generation: GenerationId,
    port: String,
    item: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileOutcome {
    #[serde(flatten)]
    source: Source,
    pub file: Option<String>,
    pub suggested_name: Option<String>,
    pub hash: Option<nodecraft_types::Blake3>,
    pub bytes: u64,
    pub state: String,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub entries: Vec<FileOutcome>,
    pub manifest: Option<String>,
    pub manifest_error: Option<String>,
    pub cancelled: bool,
}
impl Report {
    pub fn written(&self) -> usize {
        self.entries.iter().filter(|e| e.state == "written").count()
    }
    pub fn failed(&self) -> usize {
        self.entries.iter().filter(|e| e.state == "failed").count()
    }
}
fn invalid(s: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, s)
}
struct Bounded {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for Bounded {
    fn write(&mut self, part: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().checked_add(part.len()).is_none_or(|n| n > self.cap) {
            return Err(std::io::Error::other("manifest capacity"));
        }
        self.bytes.try_reserve(part.len()).map_err(|_| std::io::Error::other("manifest allocation"))?;
        self.bytes.extend_from_slice(part);
        Ok(part.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encoded(value: &impl Serialize, cap: usize) -> Result<Vec<u8>, Error> {
    let mut out = Bounded { bytes: Vec::new(), cap };
    serde_json::to_writer(&mut out, value).map_err(|_| invalid("The batch manifest is too large; select fewer results"))?;
    Ok(out.bytes)
}
fn sensitive(name: &str) -> bool {
    let key = name.to_ascii_lowercase().replace(['_', '-'], "");
    ["apikey", "authorization", "accesstoken", "refreshtoken", "password", "secret", "credential"].iter().any(|v| key.contains(v))
}
fn redact(value: &mut Json, folder: &str, depth: usize) {
    if depth > 32 {
        *value = Json::String("[unavailable]".into());
        return;
    }
    match value {
        Json::Object(map) => {
            if map.get("role").and_then(Json::as_str).is_some_and(sensitive) {
                map.insert("value".into(), Json::String("[redacted]".into()));
            }
            for (key, value) in map {
                if sensitive(key) {
                    *value = Json::String("[redacted]".into());
                } else {
                    redact(value, folder, depth + 1);
                }
            }
        }
        Json::Array(items) => {
            for value in items {
                redact(value, folder, depth + 1);
            }
        }
        Json::String(text) if !folder.is_empty() && text.contains(folder) => *text = text.replace(folder, "[destination]"),
        _ => {}
    }
}
fn provenance(generation: &Generation, fan: Option<usize>, folder: &str, remaining: &mut usize) -> Result<Json, Error> {
    let jobs: Vec<&JobSettings> = generation.settings.iter().filter(|s| fan.is_none_or(|n| s.item.is_none_or(|i| i as usize == n))).collect();
    #[derive(Serialize)]
    struct Recorded<'a> {
        origin: &'a str,
        input_hash: Option<nodecraft_types::Blake3>,
        jobs: Vec<&'a JobSettings>,
    }
    let source = Recorded { origin: if jobs.is_empty() { "not_recorded" } else { "recorded_history" }, input_hash: generation.input_hash, jobs };
    let bytes = encoded(&source, (*remaining).min(MAX_MANIFEST))?;
    let mut value: Json = serde_json::from_slice(&bytes).map_err(|_| invalid("The recorded result provenance is invalid"))?;
    redact(&mut value, folder, 0);
    let bytes = encoded(&value, *remaining)?;
    *remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| invalid("The batch provenance is too large; select fewer results"))?;
    Ok(value)
}
struct Builder<'a, 'b> {
    context: &'a Context<'b>,
    template: Template,
    entries: Vec<Entry>,
    remaining: usize,
    folder: &'a str,
}
impl Builder<'_, '_> {
    fn add(&mut self, node: &Node, generation: &Generation, port: &str, item: Option<usize>, fan: Option<usize>, value: &Value) -> Result<(), Error> {
        let Value::Media(asset) = value else {
            return Ok(());
        };
        let mime = asset.mime.essence();
        if !(mime.starts_with("image/") || mime.starts_with("audio/") || mime.starts_with("video/") || mime.starts_with("model/") || mime == "application/pdf")
        {
            return Ok(());
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err(invalid("A save includes at most 256 media files; select fewer nodes or active results"));
        }
        let source = Source { node: node.id, generation: generation.id, port: port.into(), item };
        let name = self.template.render(
            &NameFields {
                index: self.entries.len() as u32 + 1,
                node: node.id,
                generation: Some(generation.id),
                kind: &node.kind,
                title: if node.title.trim().is_empty() { &node.kind } else { &node.title },
                output: port,
                item: item.and_then(|n| u32::try_from(n).ok()),
                hash: Some(asset.hash),
            },
            mime,
        );
        let (name, error) = match name {
            Ok(name) => (Some(name), None),
            Err(_) => (None, Some("The node or output needs a safe filename; remove path separators and '..'".into())),
        };
        let error = error.or_else(|| self.context.assets.descriptor(&asset.hash).is_none().then(|| "This result's stored media is missing".into()));
        let provenance = provenance(generation, fan, self.folder, &mut self.remaining)?;
        self.entries.push(Entry { name, asset: Some(asset.clone()), source, provenance, error });
        Ok(())
    }
}
fn build(c: &Context<'_>, parameters: &Json, write: bool) -> Result<Plan, Error> {
    let request: Request = params(parameters)?;
    let targets = request.targets.as_deref().unwrap_or_default();
    if request.targets.is_some() && targets.is_empty() {
        return Err(invalid("Choose at least one explicit history result"));
    }
    if request.nodes.len() > 256 || targets.len() > MAX_ENTRIES {
        return Err(invalid("Select at most 256 nodes or explicit results"));
    }
    if targets.iter().any(|t| t.port.as_ref().is_some_and(|p| p.len() > 256) || t.item.is_some_and(|i| i > 4095)) {
        return Err(invalid("The requested output or item is out of bounds"));
    }
    if !targets.is_empty() && !request.nodes.is_empty() {
        return Err(invalid("Choose nodes or explicit history targets, not both"));
    }
    let host = c.resources.get::<ExportHost>().ok_or_else(|| Error::new(ErrorKind::Unsupported, nodecraft_format::export::reasons::NO_HOST))?;
    let destination = host.destination(&request.destination, write)?;
    let template = Template::parse(request.template.as_deref().unwrap_or(DEFAULT_TEMPLATE))?;
    let canvas = c.document.canvases().get(&request.canvas).ok_or_else(|| invalid("The canvas is no longer available"))?;
    let selected: BTreeSet<_> = request.nodes.iter().copied().collect();
    if selected.len() != request.nodes.len() || selected.iter().any(|n| !canvas.nodes().contains_key(n)) {
        return Err(invalid("The selected nodes are duplicated or no longer available"));
    }
    let mut builder = Builder { context: c, template, entries: Vec::new(), remaining: MAX_MANIFEST - 256 * 1024, folder: &request.destination };
    let mut visited = 0usize;
    if !targets.is_empty() {
        for target in targets {
            let node = canvas.nodes().get(&target.node).ok_or_else(|| invalid("The requested result node is unavailable"))?;
            let generation = node.history.iter().find(|g| g.id == target.generation).ok_or_else(|| invalid("The requested history result is unavailable"))?;
            let (value, _) = crate::history_cmds::result_value(c, request.canvas, node.id, target.generation, target.port.as_deref(), target.item)?;
            let port = target.port.as_deref().or_else(|| node.outputs.first().map(|p| p.name.as_str())).ok_or_else(|| invalid("The result has no output"))?;
            let fan = target.item.and_then(|item| {
                if generation.items.is_empty() {
                    return None;
                }
                let index = node.outputs.iter().position(|p| p.name == port)?;
                let Value::List(outer) = generation.outputs.get(index)? else {
                    return None;
                };
                let mut offset = 0usize;
                outer.iter().enumerate().find_map(|(fan, value)| {
                    let count = match value {
                        Value::List(inner) => inner.len(),
                        _ => 1,
                    };
                    let here = item >= offset && item < offset.saturating_add(count);
                    offset = offset.saturating_add(count);
                    here.then_some(fan)
                })
            });
            builder.add(node, generation, port, target.item, fan, value)?;
        }
    } else {
        for node in canvas.nodes().values().filter(|n| selected.is_empty() || selected.contains(&n.id)) {
            for generation in &node.history {
                visited = visited.checked_add(1).filter(|n| *n <= 65_536).ok_or_else(|| invalid("Too much history; select fewer nodes"))?;
                if matches!(request.history, History::Active) && Some(generation.id) != node.active {
                    continue;
                }
                for (output, value) in generation.outputs.iter().enumerate() {
                    let fallback = format!("output-{output}");
                    let port = if generation.outputs.len() == node.outputs.len() {
                        node.outputs.get(output).map(|p| p.name.as_str()).unwrap_or(&fallback)
                    } else {
                        &fallback
                    };
                    visited = visited.checked_add(1).filter(|n| *n <= 65_536).ok_or_else(|| invalid("Too many history values; select fewer nodes"))?;
                    match value {
                        Value::List(items) => {
                            let mut ordinal = 0usize;
                            for (outer, value) in items.iter().enumerate() {
                                let values = if !generation.items.is_empty() {
                                    match value {
                                        Value::List(inner) => inner.as_slice(),
                                        _ => std::slice::from_ref(value),
                                    }
                                } else {
                                    std::slice::from_ref(value)
                                };
                                for value in values {
                                    visited = visited
                                        .checked_add(1)
                                        .filter(|n| *n <= 65_536)
                                        .ok_or_else(|| invalid("Too many history values; select fewer nodes"))?;
                                    builder.add(node, generation, port, Some(ordinal), (!generation.items.is_empty()).then_some(outer), value)?;
                                    ordinal = ordinal.saturating_add(1);
                                }
                            }
                        }
                        value => builder.add(node, generation, port, None, None, value)?,
                    }
                }
            }
        }
    }
    if builder.entries.is_empty() {
        return Err(invalid("The chosen nodes/history contain no saved media results"));
    }
    Ok(Plan { host: host.clone(), destination, entries: builder.entries, scope: request.history })
}
fn preview(plan: &Plan) -> Json {
    json!({"total":plan.entries.len(),"ready":plan.entries.iter().filter(|e|e.error.is_none()).count(),"history":plan.scope,"never_overwrite":true,
        "names_may_receive_suffix":true,"manifest":"workflow-results.json","examples":plan.entries.iter().take(8).map(|e|json!({"node":e.source.node,"generation":e.source.generation,"file":e.name.as_ref().map(ExportName::file_name),"error":e.error})).collect::<Vec<_>>()})
}
fn schema() -> Json {
    let uuid = json!({"type":"string","format":"uuid"});
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"required":["canvas","destination"],"properties":{
        "canvas":uuid,"nodes":{"type":"array","maxItems":256,"items":uuid},"history":{"enum":["all","active"]},"destination":{"type":"string","minLength":1,"maxLength":4096},"template":{"type":"string","minLength":1,"maxLength":128},
        "targets":{"type":"array","maxItems":MAX_ENTRIES,"items":{"type":"object","additionalProperties":false,"required":["node","generation"],"properties":{"node":uuid,"generation":uuid,"port":{"type":"string","maxLength":256},"item":{"type":"integer","minimum":0,"maximum":4095}}}}}})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "export.results_plan",
            label: "Preview workflow result save",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "{canvas,nodes?:NodeId[] (empty=all),history?:all|active (default all),targets?:[{node,generation,port?,item?}],destination:absolute authorized folder,template?:filename tokens} -> {total,ready,examples,manifest}. Preview only; explicit targets and nodes are mutually exclusive. Stored original extensions, never overwrite, historical settings only.",
            enabled: |c, v| build(c, v, false).map(|_| ()),
            run: |c, v| build(c, v, false).map(|p| Effect::None { result: preview(&p) }),
        },
        CommandSpec {
            id: "export.results_start",
            label: "Save workflow results to folder",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: "Same params as export.results_plan -> {pending:id}. Requires a trusted host folder grant and background export runner. Saves immutable historical media plus a manifest without overwrite; export.status reports progress/outcome, export.cancel stops pending work. No dialogs, graph changes, undo or automatic replay.",
            enabled: |c, v| build(c, v, true).map(|_| ()),
            run: |c, v| build(c, v, true).map(|plan| Effect::ResultsExport { plan: Box::new(plan) }),
        },
    ]
}

#[cfg(unix)]
pub(crate) fn run(assets: &AssetStore, plan: Plan, control: &crate::export_job::Control) -> Result<Report, Error> {
    let destination = plan.host.pinned_destination(&plan.destination)?;
    let mut report = Report { entries: Vec::new(), manifest: None, manifest_error: None, cancelled: false };
    let total = plan.entries.len();
    for entry in &plan.entries {
        let mut outcome = FileOutcome {
            source: entry.source.clone(),
            file: None,
            suggested_name: entry.name.as_ref().map(ExportName::file_name),
            hash: entry.asset.as_ref().map(|a| a.hash),
            bytes: 0,
            state: "not_started".into(),
            reason: None,
        };
        if control.cancel.check().is_err() {
            report.cancelled = true;
        }
        if !report.cancelled {
            if let Some(reason) = &entry.error {
                outcome.state = "failed".into();
                outcome.reason = Some(reason.clone());
            } else if let (Some(name), Some(asset)) = (&entry.name, &entry.asset) {
                control.progress(report.entries.len() as u32, total as u32, report.written() as u32, report.failed() as u32);
                let written = publish_numbered(&destination, name, &control.cancel, |file| copy(assets, asset, file, &control.cancel));
                match written {
                    Ok((file, bytes, warning)) => {
                        outcome.file = Some(file);
                        outcome.bytes = bytes;
                        outcome.reason = warning;
                        outcome.state = "written".into();
                    }
                    Err(error) if nodecraft_format::asset_source::TransferCancel::is_cancellation(&error) => {
                        report.cancelled = true;
                    }
                    Err(error) => {
                        outcome.state = "failed".into();
                        outcome.reason = Some(error.message);
                    }
                }
            }
        }
        report.entries.push(outcome);
        control.progress(report.entries.len() as u32, total as u32, report.written() as u32, report.failed() as u32);
    }
    // Write even a cancelled batch's actual completed names, under the same grant. This
    // bounded final bookkeeping is observed by the quit watch; no further media starts.
    report.cancelled |= control.cancel.check().is_err();
    let manifest = json!({"version":1,"history":plan.scope,"cancelled":report.cancelled,"entries":report.entries.iter().zip(&plan.entries).map(|(outcome,entry)|json!({"result":outcome,"provenance":entry.provenance})).collect::<Vec<_>>()});
    let bytes = match encoded(&manifest, MAX_MANIFEST) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.manifest_error = Some(error.message);
            return Ok(report);
        }
    };
    control.finalizing();
    let final_cancel = nodecraft_format::asset_source::TransferCancel::default();
    let name = ExportName { stem: "workflow-results".into(), extension: Some("json".into()) };
    match publish_numbered(&destination, &name, &final_cancel, |file| {
        file.write_all(&bytes).map_err(|e| crate::export::io_error(e.kind()))?;
        Ok(bytes.len() as u64)
    }) {
        Ok((name, _, warning)) => {
            report.manifest = Some(name);
            report.manifest_error = warning;
        }
        Err(e) => report.manifest_error = Some(e.message),
    }
    Ok(report)
}
#[cfg(not(unix))]
pub(crate) fn run(_: &AssetStore, _: Plan, _: &crate::export_job::Control) -> Result<Report, Error> {
    Err(Error::new(ErrorKind::Unsupported, "Secure folder saving requires descriptor-relative export support; use individual Save as on this platform"))
}

#[cfg(unix)]
fn publish_numbered(
    destination: &crate::local_export::secure::Destination<'_>,
    name: &ExportName,
    cancel: &nodecraft_format::asset_source::TransferCancel,
    mut fill: impl FnMut(&mut std::fs::File) -> Result<u64, Error>,
) -> Result<(String, u64, Option<String>), Error> {
    for number in 1..=MAX_COLLISION_SUFFIX {
        cancel.check()?;
        let file = if number == 1 { name.file_name() } else { name.with_suffix(number) };
        if destination.occupied(&file)? {
            continue;
        }
        match destination.publish_new(&file, cancel, |out| fill(out)) {
            Ok((bytes, warning)) => return Ok((file, bytes, warning)),
            Err(e) if crate::export::name_conflict(&e) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(invalid("Too many files already use this name; choose a different pattern"))
}
#[cfg(unix)]
fn copy(assets: &AssetStore, asset: &AssetRef, file: &mut std::fs::File, cancel: &nodecraft_format::asset_source::TransferCancel) -> Result<u64, Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let lease = loop {
        cancel.check()?;
        match assets.open_export_verified(asset, cancel, &Default::default()) {
            Ok(lease) => break lease,
            Err(e) if e.kind == ErrorKind::Busy && std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => return Err(e),
        }
    };
    lease.copy_to(file, cancel, &Default::default())
}

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, not(unix)))]
#[test]
fn results966_non_unix_refuses_before_any_folder_write() {
    let dir = tempfile::tempdir().unwrap();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let mut session = crate::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([96; 16]), "Public workflow").unwrap()).unwrap();
    let error = crate::local_export::dispatch_with_host(
        &mut session,
        &host,
        "export.results_start",
        &json!({"canvas":CanvasId::from_bytes([96;16]),"destination":dir.path()}),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("descriptor-relative"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
