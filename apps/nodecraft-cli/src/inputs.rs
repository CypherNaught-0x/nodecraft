//! Headless project/technique targets and named values, using the canvas commands.
use nodecraft_engine::{
    Session,
    commands::Services,
    library::{FileLibrary, Library, LibraryStore},
};
use nodecraft_format::{AssetStore, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, NodeId, ParamControl, ParamSpec, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MAX_BINDINGS: usize = 256;
const MAX_FILE: usize = nodecraft_engine::import::MAX_MEDIA_BYTES;
const MAX_FILES: usize = 256 * 1024 * 1024;
const MAX_VALUE: usize = 1024 * 1024;
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Frozen source snapshot; every CSV row gets an isolated session and the same input bytes.
pub struct Target {
    snapshot: Snapshot,
    origin: Option<StorageLocation>,
    pub canvas: CanvasId,
    pub nodes: Vec<NodeId>,
    placed: Option<(CanvasId, NodeId)>,
    native_parent: Option<PathBuf>,
}
impl Target {
    pub fn open(source: &str, options: &crate::Options) -> Result<Self, Error> {
        if !source.starts_with("technique:") && !Path::new(source).is_file() {
            return Err(Error::new(ErrorKind::NotFound, "no project file at the supplied target"));
        }
        if let Some(out) = options.out.as_deref() {
            crate::run::out_folder(Path::new(out))?;
        }
        let mut ids = crate::random_bytes();
        let native_parent = options.native_assets.then(|| {
            options.out.as_deref().map(PathBuf::from).unwrap_or_else(|| {
                if source.starts_with("technique:") {
                    PathBuf::from(".")
                } else {
                    Path::new(source).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf()
                }
            })
        });
        let context = native_parent.as_deref().map(crate::project::native_context).transpose()?.unwrap_or_default();
        let (mut snapshot, origin, selector) = if let Some(selector) = source.strip_prefix("technique:") {
            if options.technique.is_some() {
                return Err(invalid("a library target already selects its technique"));
            }
            let dir = nodecraft_engine::settings::settings_dir().ok_or_else(|| invalid("the technique library folder is unavailable"))?;
            let bytes = FileLibrary::new(dir.join("library")).load()?.ok_or_else(|| invalid("the technique library is empty"))?;
            let library = Library::from_bytes(&bytes)?;
            let version = select_version(library.versions(), selector)?;
            let mut project = Project::new(ProjectId::from_bytes(ids()), "CLI technique")?;
            Change::AddCanvas { canvas: version.clone() }.apply(&mut project)?;
            (Snapshot { project, assets: AssetStore::with_context(context), epoch: 0 }, None, Some(version.id().to_string()))
        } else {
            let path = Path::new(source);
            (crate::project::snapshot_in_context(path, context)?, Some(crate::project::origin(path)?), options.technique.clone())
        };
        let (canvas, nodes, placed) = if let Some(selector) = selector {
            if options.canvas.is_some() || !options.nodes.is_empty() {
                return Err(invalid("--technique cannot be combined with --canvas or --node"));
            }
            let version = select_version(snapshot.project.canvases().values(), &selector)?.id();
            let canvas = CanvasId::from_bytes(ids());
            let node = NodeId::from_bytes(ids());
            Change::AddCanvas { canvas: Canvas::new(canvas, "CLI runs") }.apply(&mut snapshot.project)?;
            (canvas, vec![node], Some((version, node)))
        } else {
            let session = Session::new(snapshot.project.clone())?;
            let canvas = crate::quote::canvas_of(&session, options.canvas.as_deref())?;
            // Validate explicit targets now; all-stale targets are resolved again after binding.
            crate::quote::targets(&session, canvas, &options.nodes)?;
            (canvas, options.nodes.clone(), None)
        };
        Ok(Self { snapshot, origin, canvas, nodes, placed, native_parent })
    }
    pub fn session(&self, services: Services) -> Result<Session, Error> {
        let store = match &self.native_parent {
            Some(parent) => crate::project::ephemeral(parent)?,
            None => Box::new(nodecraft_format::MemoryStorage::default()),
        };
        let mut session = match &self.origin {
            Some(origin) => Session::with_storage_origin(self.snapshot.clone(), store, origin.clone())?,
            None => Session::with_storage(self.snapshot.clone(), store)?,
        }
        .with_services(services);
        if let Some((version, id)) = self.placed {
            session.dispatch("technique.place", &json!({"canvas":self.canvas,"version":version,"id":id,"pos":{"x":0,"y":0}}))?;
        }
        Ok(session)
    }
    pub fn bindings(&self, raw: &[String], base: &Path, files: &mut Files) -> Result<Vec<Binding>, Error> {
        if raw.len() > MAX_BINDINGS {
            return Err(invalid("at most 256 named inputs may be supplied"));
        }
        let mut session = self.session(Services::default())?;
        let form_node = if let [id] = self.nodes.as_slice() {
            session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(id)).filter(|n| n.kind == nodecraft_nodes::technique::ID).map(|n| n.id)
        } else {
            None
        };
        let form = form_node.map(|id| session.dispatch("app.form", &json!({"canvas":self.canvas,"id":id}))).transpose()?;
        let mut seen = BTreeSet::new();
        let mut bindings = Vec::new();
        let mut text_bytes = 0usize;
        for assignment in raw {
            text_bytes = text_bytes.saturating_add(assignment.len());
            if text_bytes > 8 * MAX_VALUE {
                return Err(invalid("named input text exceeds 8 MiB"));
            }
            let (key, value) = assignment.split_once('=').ok_or_else(|| invalid("--input requires name=value"))?;
            if key.is_empty() || key.len() > 256 || value.len() > MAX_VALUE {
                return Err(invalid("input name or value exceeds its limit"));
            }
            let dest = resolve(&session, self.canvas, form_node, form.as_ref(), key)?;
            if !seen.insert(dest.identity()) {
                return Err(invalid(format!("input `{key}` is supplied more than once")));
            }
            let values = dest.parse(value, base, files)?;
            let binding = Binding { dest: dest.address(), values };
            // Validate all bindings through the actual engine path before any provider setup.
            binding.apply(&mut session, self.canvas)?;
            bindings.push(binding);
        }
        Ok(bindings)
    }
}
pub fn open_session(path: &Path, options: &crate::Options, services: Services) -> Result<(Session, CanvasId, Vec<NodeId>), Error> {
    let source = path.to_str().ok_or_else(|| invalid("the target must be UTF-8"))?;
    if !options.native_assets && !source.starts_with("technique:") && options.technique.is_none() && options.inputs.is_empty() {
        let session = crate::project::open_with_assets(path)?.with_services(services);
        let canvas = crate::quote::canvas_of(&session, options.canvas.as_deref())?;
        crate::quote::targets(&session, canvas, &options.nodes)?;
        return Ok((session, canvas, options.nodes.clone()));
    }
    let target = Target::open(source, options)?;
    let bindings = target.bindings(&options.inputs, Path::new("."), &mut Files::default())?;
    let mut session = target.session(services)?;
    for binding in &bindings {
        binding.apply(&mut session, target.canvas)?;
    }
    Ok((session, target.canvas, target.nodes.clone()))
}
fn select_version<'a>(canvases: impl Iterator<Item = &'a Canvas>, selector: &str) -> Result<&'a Canvas, Error> {
    let (wanted, version) = match selector.rsplit_once('@') {
        Some((name, version)) => (name, Some(version.parse::<u32>().map_err(|_| invalid("technique version must be a positive integer"))?)),
        None => (selector, None),
    };
    let mut matches = canvases
        .filter(|c| {
            c.technique_info().is_some_and(|i| {
                !i.retired && (c.id().to_string() == wanted || i.id.to_string() == wanted || i.name == wanted) && version.is_none_or(|v| i.version == v)
            })
        })
        .collect::<Vec<_>>();
    let ids: BTreeSet<_> = matches.iter().filter_map(|c| c.technique_info().map(|i| i.id)).collect();
    if ids.len() > 1 {
        return Err(invalid("the technique name is ambiguous; use its id"));
    }
    matches.sort_by_key(|c| c.technique_info().map(|i| i.version));
    matches.last().copied().ok_or_else(|| Error::new(ErrorKind::NotFound, "no matching published technique version"))
}

#[derive(Clone)]
enum Dest {
    Input { node: NodeId, port: String, modality: Modality, many: bool },
    Param { node: NodeId, spec: Box<ParamSpec> },
}
impl Dest {
    fn address(&self) -> Address {
        match self {
            Self::Input { node, port, .. } => Address::Input { node: *node, port: port.clone() },
            Self::Param { node, spec } => Address::Param { node: *node, name: spec.name.clone() },
        }
    }
    fn identity(&self) -> (NodeId, String) {
        match self {
            Self::Input { node, port, .. } => (*node, format!("input:{port}")),
            Self::Param { node, spec } => (*node, format!("param:{}", spec.name)),
        }
    }
    fn parse(&self, text: &str, base: &Path, files: &mut Files) -> Result<Vec<Given>, Error> {
        match self {
            Self::Input { modality, many, .. } => {
                let texts = if *many && text.starts_with("json:") {
                    serde_json::from_str::<Vec<String>>(text.strip_prefix("json:").unwrap_or_default())
                        .map_err(|_| invalid("a list input requires json:[string,...]"))?
                } else {
                    vec![text.to_owned()]
                };
                if texts.len() > 64 {
                    return Err(invalid("one input takes at most 64 values"));
                }
                texts
                    .into_iter()
                    .map(|s| if *modality == Modality::Text { Ok(Given::Value(Value::Text(s))) } else { files.media(base, &s, *modality) })
                    .collect()
            }
            Self::Param { spec, .. } => {
                let given = if let ParamControl::Asset { modality } = spec.control {
                    files.media(base, text, modality)?
                } else {
                    Given::Value(typed(&spec.control, text)?)
                };
                Ok(vec![given])
            }
        }
    }
}
fn resolve(session: &Session, canvas: CanvasId, form_node: Option<NodeId>, form: Option<&Json>, key: &str) -> Result<Dest, Error> {
    if let (Some(node), Some(form)) = (form_node, form) {
        let (input_name, param_name) = if let Some(s) = key.strip_prefix("input:") {
            (Some(s), None)
        } else if let Some(s) = key.strip_prefix("param:") {
            (None, Some(s))
        } else {
            (Some(key), Some(key))
        };
        let mut found = Vec::new();
        for input in form["inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| input_name.is_some_and(|k| i["port"].as_str() == Some(k) || i["name"].as_str() == Some(k)))
        {
            if input["fillable"] != true {
                return Err(invalid("this technique input must be connected on the canvas"));
            }
            let kind: nodecraft_types::PortType = serde_json::from_value(input["port_type"].clone()).map_err(|_| invalid("invalid technique input type"))?;
            let port = input["port"].as_str().ok_or_else(|| invalid("invalid technique input port"))?.to_owned();
            found.push(Dest::Input { node, port, modality: kind.modality, many: kind.many });
        }
        for parameter in form["params"].as_array().into_iter().flatten() {
            let spec: ParamSpec = serde_json::from_value(parameter["spec"].clone()).map_err(|_| invalid("invalid technique parameter"))?;
            if param_name.is_some_and(|name| name == spec.name || name == spec.label) {
                found.push(Dest::Param { node, spec: Box::new(spec) });
            }
        }
        return match found.len() {
            1 => found.pop().ok_or_else(|| invalid("missing input")),
            0 => Err(invalid(format!("no technique input or parameter named `{key}`"))),
            _ => Err(invalid("ambiguous input name; use input:port or param:name")),
        };
    }
    let graph = session.document().canvases().get(&canvas).ok_or_else(|| invalid("no such canvas"))?;
    let exact = graph.nodes().values().any(|n| n.id.to_string() == key || n.title == key);
    let (node_key, param) = if exact { (key, None) } else { key.rsplit_once('.').map_or((key, None), |(node, param)| (node, Some(param))) };
    let nodes: Vec<_> = graph.nodes().values().filter(|n| n.id.to_string() == node_key || n.title == node_key).collect();
    let [node] = nodes.as_slice() else {
        return Err(invalid("input must name one source node, or node-id.parameter; use --node for a technique form"));
    };
    let name = match param {
        Some(p) => p,
        None => match node.kind.as_str() {
            "source.text" => "text",
            "source.image" | "source.video" | "source.audio" | "source.document" | "source.mesh" | "source.style" => "asset",
            _ => return Err(invalid("name an explicit node parameter for a non-source input")),
        },
    };
    let descriptor = session.kinds().get(&node.kind)?.resolve_instance(node)?;
    let spec = descriptor.params.params.iter().find(|s| s.name == name).cloned().ok_or_else(|| invalid("the node has no such parameter"))?;
    Ok(Dest::Param { node: node.id, spec: Box::new(spec) })
}
fn typed(control: &ParamControl, text: &str) -> Result<Value, Error> {
    let value = if matches!(control, ParamControl::Text { .. } | ParamControl::Choice { .. } | ParamControl::Model) {
        Value::Text(text.to_owned())
    } else if let Some(raw) = text.strip_prefix("json:") {
        serde_json::from_str(raw).map_err(|_| invalid("invalid typed JSON input"))?
    } else {
        let kind = match control {
            ParamControl::Text { .. } | ParamControl::Choice { .. } | ParamControl::Model => "text",
            ParamControl::Toggle => "bool",
            ParamControl::Slider { .. } => "number",
            ParamControl::Integer { .. } => "integer",
            ParamControl::Unsigned { .. } | ParamControl::Seed { .. } => "unsigned",
            ParamControl::Color => return Err(invalid("colors require json: followed by a typed list value")),
            ParamControl::Asset { .. } => return Err(invalid("media inputs require a file path")),
        };
        let payload = if kind == "bool" { json!(text.parse::<bool>().map_err(|_| invalid("a toggle takes true or false"))?) } else { json!(text) };
        serde_json::from_value(json!({"type":kind,"value":payload})).map_err(|_| invalid("value does not match the parameter's type"))?
    };
    control.validate_value(&value)?;
    Ok(value)
}
#[derive(Clone)]
enum Given {
    Value(Value),
    Media { name: String, bytes: Arc<[u8]>, modality: Modality },
}
/// Media paths are snapshotted once, before any row can submit paid work.
#[derive(Default)]
pub struct Files {
    cache: BTreeMap<PathBuf, Arc<[u8]>>,
    bytes: usize,
}
impl Files {
    fn media(&mut self, base: &Path, text: &str, modality: Modality) -> Result<Given, Error> {
        let raw = text.strip_prefix('@').unwrap_or(text);
        let path = base.join(raw);
        let path = path.canonicalize().map_err(|_| Error::new(ErrorKind::NotFound, "an input file cannot be resolved"))?;
        let bytes = if let Some(bytes) = self.cache.get(&path) {
            bytes.clone()
        } else {
            if self.cache.len() >= 1024 {
                return Err(invalid("input file count exceeds 1024"));
            }
            if !path.is_file() {
                return Err(invalid("a media input must be a regular file"));
            }
            let file = std::fs::File::open(&path).map_err(|_| invalid("an input file cannot be opened"))?;
            if !file.metadata().map_err(|_| invalid("input metadata unavailable"))?.is_file() {
                return Err(invalid("a media input must be a regular file"));
            }
            let mut bytes = Vec::new();
            file.take((MAX_FILE + 1) as u64).read_to_end(&mut bytes).map_err(|_| invalid("input file cannot be read"))?;
            if bytes.len() > MAX_FILE || self.bytes.saturating_add(bytes.len()) > MAX_FILES {
                return Err(invalid("input files exceed the 16 MiB per-file or 256 MiB total limit"));
            }
            self.bytes = self.bytes.saturating_add(bytes.len());
            let bytes: Arc<[u8]> = bytes.into();
            self.cache.insert(path.clone(), bytes.clone());
            bytes
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("input").to_owned();
        Ok(Given::Media { name, bytes, modality })
    }
}
#[derive(Clone)]
enum Address {
    Input { node: NodeId, port: String },
    Param { node: NodeId, name: String },
}
#[derive(Clone)]
pub struct Binding {
    dest: Address,
    values: Vec<Given>,
}
impl Binding {
    pub fn text_bytes(&self) -> usize {
        self.values
            .iter()
            .map(|value| match value {
                Given::Value(value) => serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len()),
                Given::Media { name, .. } => name.len(),
            })
            .fold(0, usize::saturating_add)
    }
    pub fn apply(&self, session: &mut Session, canvas: CanvasId) -> Result<(), Error> {
        let mut values = Vec::new();
        for value in &self.values {
            values.push(match value {
                Given::Value(value) => value.clone(),
                Given::Media { name, bytes, modality } => {
                    let asset = nodecraft_engine::import::import_media(session, &nodecraft_engine::import::Incoming { name, bytes })?;
                    if !modality.accepts_mime(asset.mime.essence()) {
                        return Err(invalid("the input file has the wrong media type"));
                    }
                    Value::Media(asset)
                }
            });
        }
        match &self.dest {
            Address::Input { node, port } => {
                let mut id = crate::random_bytes();
                session.dispatch("app.input_set", &json!({"canvas":canvas,"id":node,"port":port,"seed":NodeId::from_bytes(id()),"values":values}))?;
            }
            Address::Param { node, name } => {
                let [value] = values.as_slice() else {
                    return Err(invalid("a parameter takes exactly one value"));
                };
                session.dispatch("node.param_set", &json!({"canvas":canvas,"id":node,"name":name,"value":value}))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
