//! Per-origin provider addresses (#721). Keys stay in OriginStore; this record contains only
//! validated server addresses and the installation salt. The host's synchronous storage keeps
//! command dispatch immediate without introducing a second document mutation path.
//!
//! The browser keeps one ComfyUI server: `comfyui_url` is stored as the only, default entry of
//! the settings' server list (`[[providers.comfyui]]`, #864), named by its host and port. A
//! record saved before the list existed still holds `comfyui_url`, which reading turns into it.
use crate::commands::{CommandSpec, Context, Effect, Services, params};
use crate::secrets::OriginStorage;
use crate::settings::{ComfyServer, ProviderSettings, server_name_for};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const STORAGE_KEY: &str = "nodecraft.provider-settings.v1";
const MAX_SETTINGS_BYTES: usize = 8192;
const MAX_URL_BYTES: usize = 2048;
pub const SERVICE: &str = "providers.settings";

fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "Use an HTTP or HTTPS server address without a user name, password, query or fragment; keep tokens in API keys.")
}

/// Validate before persistence or adapter construction, without echoing credentials in errors.
pub fn server_url(value: Option<&str>) -> Result<Option<String>, Error> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else { return Ok(None) };
    if value.len() > MAX_URL_BYTES || value.chars().any(char::is_control) {
        return Err(invalid());
    }
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if url.as_str().len() > MAX_URL_BYTES
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(Some(url.as_str().trim_end_matches('/').to_owned()))
}

/// The ComfyUI address the browser shows and saves: the default server's.
pub fn comfyui_url(settings: &ProviderSettings) -> Option<String> {
    settings.default_comfy().map(|server| server.url.clone())
}

/// Makes `url` the only ComfyUI server and the default, or clears the servers for `None`.
fn set_comfyui_url(settings: &mut ProviderSettings, url: Option<String>) {
    match url {
        Some(url) => {
            let name = server_name_for(&url);
            settings.comfyui = vec![ComfyServer::new(&name, &url)];
            settings.comfyui_default = Some(name);
        }
        None => {
            settings.comfyui.clear();
            settings.comfyui_default = None;
        }
    }
}

#[derive(Clone)]
pub struct OriginProviders {
    storage: Arc<dyn OriginStorage>,
    settings: Arc<Mutex<ProviderSettings>>,
}

impl OriginProviders {
    /// Damaged/unavailable storage leaves an editable default configuration and a value-free
    /// notice. It is replaced only after an explicit successful Save.
    pub fn load(storage: Arc<dyn OriginStorage>) -> (Self, Option<String>) {
        let read = storage.get(STORAGE_KEY).map_err(|_| "This browser couldn't read provider settings; allow site data, then save the server addresses again.");
        let loaded = read.and_then(|stored| match stored {
            None => Ok(ProviderSettings::default()),
            Some(bytes) => {
                if bytes.len() > MAX_SETTINGS_BYTES {
                    return Err("Provider settings in this site's storage are damaged. Set the server addresses again.");
                }
                let mut settings: ProviderSettings =
                    serde_json::from_str(&bytes).map_err(|_| "Provider settings in this site's storage are damaged. Set the server addresses again.")?;
                let comfyui = server_url(comfyui_url(&settings).as_deref())
                    .map_err(|_| "Provider settings in this site's storage contain an unusable address. Set the server addresses again.")?;
                set_comfyui_url(&mut settings, comfyui);
                settings.litellm_url = server_url(settings.litellm_url.as_deref())
                    .map_err(|_| "Provider settings in this site's storage contain an unusable address. Set the server addresses again.")?;
                if settings.install_salt.len() > 128 {
                    return Err("Provider settings in this site's storage are damaged. Set the server addresses again.");
                }
                Ok(settings)
            }
        });
        let (settings, notice) = match loaded {
            Ok(settings) => (settings, None),
            Err(notice) => (ProviderSettings::default(), Some(notice.to_owned())),
        };
        (Self { storage, settings: Arc::new(Mutex::new(settings)) }, notice)
    }

    pub fn settings(&self) -> Result<ProviderSettings, Error> {
        self.settings.lock().map(|settings| settings.clone()).map_err(|_| Error::new(ErrorKind::Internal, "provider settings are busy; reload this page"))
    }

    fn save(&self, request: Addresses) -> Result<ProviderSettings, Error> {
        let comfyui_url = server_url(request.comfyui_url.as_deref())?;
        let litellm_url = server_url(request.litellm_url.as_deref())?;
        let mut current = self.settings.lock().map_err(|_| Error::new(ErrorKind::Internal, "provider settings are busy; reload this page"))?;
        let mut next = current.clone();
        set_comfyui_url(&mut next, comfyui_url);
        next.litellm_url = litellm_url;
        next.ensure_install_salt()?;
        let bytes = serde_json::to_string(&next).map_err(|_| Error::new(ErrorKind::Internal, "provider settings could not be encoded"))?;
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(invalid());
        }
        self.storage.set(STORAGE_KEY, &bytes).map_err(|_| {
            Error::new(ErrorKind::Unsupported, "This browser couldn't save provider settings. Allow site data or free storage, then try Save again.")
        })?;
        *current = next.clone();
        Ok(next)
    }

    pub fn register(&self, services: &mut Services) -> Result<(), Error> {
        let store = self.clone();
        services.register(SERVICE, move |request| {
            let request: Request = params(request)?;
            let settings = match request {
                Request::Get => store.settings()?,
                Request::Set { comfyui_url, litellm_url } => store.save(Addresses { comfyui_url, litellm_url })?,
            };
            Ok(json!({"comfyui_url": comfyui_url(&settings), "litellm_url": settings.litellm_url}))
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Addresses {
    comfyui_url: Option<String>,
    litellm_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Get,
    Set { comfyui_url: Option<String>, litellm_url: Option<String> },
}

fn enabled(c: &Context<'_>) -> Result<(), Error> {
    if !c.services.contains(SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host doesn't keep per-site provider settings"));
    }
    Ok(())
}

fn enabled_get(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: Empty = params(value)?;
    enabled(c)
}

fn get(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_get(c, value)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "get"}) })
}

fn enabled_set(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let addresses: Addresses = params(value)?;
    server_url(addresses.comfyui_url.as_deref())?;
    server_url(addresses.litellm_url.as_deref())?;
    enabled(c)
}

fn set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_set(c, value)?;
    let addresses: Addresses = params(value)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "set", "comfyui_url": addresses.comfyui_url, "litellm_url": addresses.litellm_url}) })
}

fn get_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

fn set_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "additionalProperties": false, "required": [], "properties": {
        "comfyui_url": {"type": ["string", "null"], "maxLength": MAX_URL_BYTES},
        "litellm_url": {"type": ["string", "null"], "maxLength": MAX_URL_BYTES}
    }})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "providers.settings_get",
            label: "Provider server settings",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: get_schema,
            params_doc: "{} -> {comfyui_url, litellm_url}; read the browser site's saved provider server addresses. No keys or installation salt are returned.",
            enabled: enabled_get,
            run: get,
        },
        CommandSpec {
            id: "providers.settings_set",
            label: "Save provider servers",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{comfyui_url?: string|null, litellm_url?: string|null} -> {comfyui_url, litellm_url}; replace both server addresses in this browser site's storage. Omitted, null or blank values clear an address. HTTP(S) only; credentials, queries and fragments refuse. Keys belong in keys.set. Failed persistence preserves the prior settings.",
            enabled: enabled_set,
            run: set,
        },
    ]
}

#[cfg(test)]
mod tests;
