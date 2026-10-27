//! Headless ComfyUI discovery and workflow import, through the engine's commands.
use nodecraft_engine::{
    Session, comfy_server_cmds, comfy_workflow_cmds as workflow, provider_setup::HostEnv, secrets::KeyringStore, settings::ProviderSettings,
};
use nodecraft_format::{
    ArchivePlan, AssetStore, Snapshot, StorageCapability,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
/// Settings may contain an unchecked URL. Neither listing mode may echo credentials.
fn shown_url(text: &str) -> Option<String> {
    if text.len() > 2048 {
        return None;
    }
    let mut url = url::Url::parse(text).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}
const MAX_WORKFLOW_BYTES: usize = 1024 * 1024;

fn fetcher(mut settings: ProviderSettings, name: &str) -> Result<impl FnMut() -> Result<workflow::Fetched, Error> + 'static, Error> {
    settings.comfyui_default = Some(name.into());
    let http = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|_| invalid("cannot create the HTTP client"))?);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| invalid("cannot start ComfyUI discovery"))?;
    let start = Instant::now();
    let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
    Ok(move || {
        let env = HostEnv { keys: &KeyringStore, http: http.clone(), settings: &settings, now_ms: now_ms.clone(), progress: None };
        runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(60), workflow::fetch(&env)).await })
            .map_err(|_| Error::new(ErrorKind::Busy, "ComfyUI discovery timed out after 60 seconds"))?
    })
}

/// The same refresh service/finish path as the desktop, without a UI or generation request.
fn refresh(session: &mut Session, name: &str, mut fetch: impl FnMut() -> Result<workflow::Fetched, Error> + 'static) -> Result<workflow::Report, Error> {
    let result = Arc::new(Mutex::new(None));
    let returned = result.clone();
    session.register_host_service(workflow::NODES_SERVICE, move |_| {
        *returned.lock().map_err(|_| invalid("ComfyUI discovery result unavailable"))? = Some(fetch());
        Ok(json!({"pending":1}))
    })?;
    session.dispatch("comfy.nodes.refresh", &json!({"server":name}))?;
    let fetched =
        result.lock().map_err(|_| invalid("ComfyUI discovery result unavailable"))?.take().ok_or_else(|| invalid("ComfyUI discovery did not return"))?;
    Ok(comfy_server_cmds::finish(session, 1, name, true, fetched))
}
fn blank() -> Result<Session, Error> {
    let mut id = crate::random_bytes();
    nodecraft_automation::blank_session(ProjectId::from_bytes(id()), CanvasId::from_bytes(id()))
}
fn servers(args: &[String]) -> Result<ExitCode, Error> {
    let mut test = false;
    let mut json = false;
    let mut name = None;
    for arg in args {
        match arg.as_str() {
            "--test" => test = true,
            "--json" => json = true,
            flag if flag.starts_with('-') => return Err(invalid("comfy servers takes only [name], --test and --json")),
            _ if name.is_none() => name = Some(arg.as_str()),
            _ => return Err(invalid("comfy servers takes at most one server name")),
        }
    }
    let settings = nodecraft_engine::settings::peek().providers;
    let entries = settings.comfy_servers();
    if name.is_some_and(|name| settings.comfy_server(name).is_none()) {
        return Err(invalid("no configured ComfyUI server has that name"));
    }
    let default = settings.default_comfy().map(|s| s.name.as_str());
    let mut reports = Vec::new();
    let mut failed = false;
    for server in entries.into_iter().filter(|s| name.is_none_or(|name| name == s.name)) {
        let mut row = json!({"name":server.name,"url":shown_url(&server.url),"backend":server.backend,"default":Some(server.name.as_str())==default,"max_jobs":server.max_jobs()});
        if test {
            let mut session = blank()?;
            let checked = refresh(&mut session, &server.name, fetcher(settings.clone(), &server.name)?);
            match checked {
                Ok(report) => {
                    failed |= report.error.is_some();
                    row["check"] = json!(report);
                }
                Err(error) => {
                    failed = true;
                    row["check"] = json!({"error":error.message});
                }
            }
        }
        reports.push(row);
    }
    let value = json!(reports);
    let code = crate::print(json, &value, |value| {
        let mut text = String::new();
        for server in value.as_array().into_iter().flatten() {
            text.push_str(&format!(
                "{}{}: {}\n",
                server["name"].as_str().unwrap_or_default(),
                if server["default"] == true { " (default)" } else { "" },
                server["url"].as_str().unwrap_or("invalid configured URL")
            ));
            if let Some(error) = server["check"]["error"].as_str() {
                text.push_str(&format!("  failed: {error}\n"));
            } else if test {
                text.push_str(&format!("  connected; {} node classes\n", server["check"]["nodes"]));
            }
        }
        if text.is_empty() {
            text.push_str("No ComfyUI servers configured. Add one in NodeCraft settings.\n");
        }
        text
    });
    Ok(if failed { ExitCode::FAILURE } else { code })
}
struct ImportArgs {
    workflow: PathBuf,
    out: PathBuf,
    project: Option<PathBuf>,
    canvas: Option<String>,
    server: Option<String>,
    title: Option<String>,
    params: Vec<String>,
    json: bool,
}
fn import_args(args: &[String]) -> Result<ImportArgs, Error> {
    let mut flags = BTreeMap::new();
    let mut params = Vec::new();
    let mut json = false;
    let mut workflow = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--out" | "--project" | "--canvas" | "--server" | "--title" | "--param" => {
                let value = iter.next().ok_or_else(|| invalid(format!("{arg} needs a value")))?.clone();
                if arg == "--param" {
                    if params.len() >= 256 || value.len() > 512 {
                        return Err(invalid("name at most 256 bounded node.input parameters"));
                    }
                    params.push(value);
                } else if flags.insert(arg.as_str(), value).is_some() {
                    return Err(invalid(format!("{arg} is given twice")));
                }
            }
            flag if flag.starts_with('-') => return Err(invalid(format!("unknown comfy import option {flag}"))),
            _ if workflow.is_none() => workflow = Some(PathBuf::from(arg)),
            _ => return Err(invalid("comfy import takes one workflow file")),
        }
    }
    let out = flags.remove("--out").ok_or_else(|| invalid("comfy import needs --out <new.ncraft>"))?.into();
    let title = flags.remove("--title");
    if title.as_ref().is_some_and(|title| title.chars().count() > 256) {
        return Err(invalid("the title exceeds 256 characters"));
    }
    Ok(ImportArgs {
        workflow: workflow.ok_or_else(|| invalid("comfy import needs a workflow JSON file"))?,
        out,
        project: flags.remove("--project").map(Into::into),
        canvas: flags.remove("--canvas"),
        server: flags.remove("--server"),
        title,
        params,
        json,
    })
}
/// Create the publication handle before any server request. Persist refuses a raced-in file too.
struct Destination {
    file: tempfile::NamedTempFile,
    path: PathBuf,
}
impl Destination {
    fn new(path: &Path) -> Result<Self, Error> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .map_err(|_| invalid("the output parent folder does not exist"))?;
        let name = path.file_name().ok_or_else(|| invalid("the output must name a new project file"))?;
        let path = parent.join(name);
        if path.symlink_metadata().is_ok() {
            return Err(Error::new(ErrorKind::Conflict, "the output already exists; choose a new file"));
        }
        let file = tempfile::NamedTempFile::new_in(&parent).map_err(|_| invalid("the output folder is not writable"))?;
        Ok(Self { file, path })
    }
    fn save(mut self, session: &Session) -> Result<PathBuf, Error> {
        let snapshot = Snapshot { project: session.document().clone(), assets: session.assets().clone(), epoch: 0 };
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
        let plan = ArchivePlan::prepare(&snapshot, StorageCapability::StreamingV2, &cancel, &metrics)?;
        plan.write_to(self.file.as_file_mut(), &cancel, &metrics)?;
        self.file.as_file().sync_all().map_err(|_| invalid("cannot sync the imported project"))?;
        self.file
            .persist_noclobber(&self.path)
            .map_err(|_| Error::new(ErrorKind::Conflict, "cannot publish the project; an existing output is never replaced"))?;
        Ok(self.path)
    }
}
fn import(args: &ImportArgs, settings: &ProviderSettings, fetch: impl FnMut() -> Result<workflow::Fetched, Error> + 'static) -> Result<Value, Error> {
    let server = match args.server.as_deref() {
        Some(name) => settings.comfy_server(name),
        None => settings.default_comfy(),
    }
    .ok_or_else(|| invalid("choose a configured ComfyUI server in NodeCraft settings"))?;
    if !args.workflow.is_file() {
        return Err(invalid("the workflow must be a regular JSON file"));
    }
    let input = std::fs::File::open(&args.workflow).map_err(|_| invalid("cannot open the workflow"))?;
    let mut text = String::new();
    input.take((MAX_WORKFLOW_BYTES + 1) as u64).read_to_string(&mut text).map_err(|_| invalid("the workflow must be readable UTF-8"))?;
    if text.len() > MAX_WORKFLOW_BYTES {
        return Err(invalid("workflow exceeds the CLI's 1 MiB engine command limit"));
    }
    if !workflow::is_workflow(&text) {
        return Err(invalid("the file is not a bounded ComfyUI workflow in API or UI format"));
    }
    let destination = Destination::new(&args.out)?;
    let parent = destination.path.parent().ok_or_else(|| invalid("the output has no parent"))?;
    let context = crate::project::native_context(parent)?;
    let mut ids = crate::random_bytes();
    let mut session = match &args.project {
        Some(path) => Session::with_storage_origin(
            crate::project::snapshot_in_context(path, context.clone())?,
            crate::project::ephemeral(parent)?,
            crate::project::origin(path)?,
        )?,
        None => {
            let mut project = Project::new(ProjectId::from_bytes(ids()), "Imported ComfyUI workflow")?;
            Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes(ids()), "Workflow") }.apply(&mut project)?;
            Session::with_storage(Snapshot { project, assets: AssetStore::with_context(context.clone()), epoch: 0 }, crate::project::ephemeral(parent)?)?
        }
    };
    let canvas = crate::quote::canvas_of(&session, args.canvas.as_deref())?;
    let id = NodeId::from_bytes(ids());
    let mut params = json!({"canvas":canvas,"id":id,"pos":{"x":0,"y":0},"workflow":text,"title":args.title});
    if !args.params.is_empty() {
        params["params"] = json!(args.params);
    }
    nodecraft_engine::commands::validate_json(&params)?;
    let report = refresh(&mut session, &server.name, fetch)?;
    if let Some(error) = report.error {
        return Err(Error::new(ErrorKind::Unsupported, error));
    }
    let imported = session.dispatch("comfy.workflow.import", &params)?;
    if args.server.is_some() {
        session.dispatch("comfy.server.use", &json!({"canvas":canvas,"id":id,"server":server.name}))?;
    }
    let path = destination.save(&session)?;
    Ok(json!({"path":path,"canvas":canvas,"node":id,"server":server.name,"import":imported}))
}
pub fn command(args: &[String]) -> ExitCode {
    let result = match args.first().map(String::as_str) {
        Some("servers") => servers(args.get(1..).unwrap_or_default()),
        Some("import") => (|| {
            let args = import_args(args.get(1..).unwrap_or_default())?;
            let settings = nodecraft_engine::settings::peek().providers;
            let name =
                args.server.as_deref().or_else(|| settings.default_comfy().map(|s| s.name.as_str())).ok_or_else(|| invalid("no ComfyUI server configured"))?;
            if settings.comfy_server(name).is_none() {
                return Err(invalid("no configured ComfyUI server has that name"));
            }
            let result = import(&args, &settings, fetcher(settings.clone(), name)?)?;
            Ok(crate::print(args.json, &result, |v| {
                format!("Imported workflow as {} in {}\n", v["node"].as_str().unwrap_or_default(), v["path"].as_str().unwrap_or_default())
            }))
        })(),
        _ => Err(invalid("comfy takes servers [name] [--test] [--json], or import <workflow.json> --out <new.ncraft>")),
    };
    result.unwrap_or_else(|error| crate::fail(&error.message))
}
#[cfg(test)]
mod tests;
