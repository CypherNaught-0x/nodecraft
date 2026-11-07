//! Live provider model lists in the CLI: a project whose cards name a model OpenRouter or a
//! LiteLLM proxy lists (`openrouter.…`, `litellm.…`) reads those lists before it is planned, as
//! the desktop does at start, once per process. Other projects send nothing. `serve` doesn't read
//! them yet, so a served Technique naming a listed model is refused.
use std::sync::Arc;
use std::time::Duration;

use nodecraft_engine::Session;
use nodecraft_engine::model_catalogs;
use nodecraft_engine::provider_setup::HostEnv;
use nodecraft_engine::secrets::KeyringStore;
use nodecraft_types::{Error, ErrorKind, Value};

/// Whether any card names a listed model (by its catalog id prefix).
fn names_listed(session: &Session) -> bool {
    let prefixes = ["openrouter.", "litellm."];
    session
        .document()
        .canvases()
        .values()
        .flat_map(|canvas| canvas.nodes().values())
        .any(|node| matches!(node.params.get(nodecraft_nodes::generation::MODEL), Some(Value::Text(model)) if prefixes.iter().any(|p| model.starts_with(p))))
}

/// Reads the lists when the project needs them. A failure is reported on stderr and the run goes
/// on: a card whose model wasn't listed is then refused by the planner with its own reason.
pub fn install_if_named(session: &mut Session) {
    if !names_listed(session) {
        return;
    }
    if let Err(error) = install(session) {
        eprintln!("warning: provider model lists couldn't be read: {}", error.message);
    }
}

/// The lists, read once per process: a run opens several sessions (bindings, the run itself,
/// each batch row), and they must all see the same models and prices.
static FETCHED: std::sync::OnceLock<Result<model_catalogs::Fetched, Error>> = std::sync::OnceLock::new();

fn install(session: &mut Session) -> Result<(), Error> {
    let fetched = FETCHED.get_or_init(read).clone()?;
    let report = model_catalogs::finish(session, 1, fetched);
    if let Some(error) = report.error.clone().or_else(|| report.sources.iter().find_map(|source| source.error.clone())) {
        return Err(Error::new(ErrorKind::Unsupported, error));
    }
    Ok(())
}

fn read() -> Result<model_catalogs::Fetched, Error> {
    let saved = nodecraft_engine::settings::peek();
    let openrouter = saved.keys.accounts().iter().any(|account| account.as_str() == "openrouter");
    let http: Arc<dyn nodecraft_providers::http::Http> =
        Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Error::new(ErrorKind::Unsupported, "can't start reading the model lists"))?;
    let env = HostEnv { keys: &KeyringStore, http, settings: &saved.providers, now_ms: Arc::new(|| 0), progress: None };
    let fetched = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(90), model_catalogs::fetch(&env, openrouter)).await })
        .map_err(|_| Error::new(ErrorKind::Timeout, "reading the model lists timed out after 90 seconds"))?;
    Ok(fetched)
}
