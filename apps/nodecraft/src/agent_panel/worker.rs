//! The agent's requests to its chat endpoint, sent on a thread of their own so a slow model or a
//! keychain prompt never holds up a frame. The thread reads the endpoint's key when it sends, and
//! hands each reply back with the ticket it answers. A request the agent no longer wants (cancelled,
//! cleared) is dropped before it is sent, and abandoned while it is being sent, so it costs nothing
//! more and doesn't hold up the next one.
use nodecraft_agent::Ticket;
use nodecraft_engine::secrets::{Account, SecretStore};
use nodecraft_engine::settings::{AgentEndpoint, AgentSettings, ProviderSettings};
use nodecraft_providers::ProviderError;
use nodecraft_providers::base_url::BaseUrl;
use nodecraft_providers::llm::{ApiKey, ChatRequest, ChatResponse, Endpoint, LlmClient};
use nodecraft_types::{Error, ErrorKind};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Sends one request with the given settings, giving up once `wanted` says no; the app's reads the
/// keychain and the network, tests pass a fixture.
pub type SendFn = Box<dyn FnMut(&AgentSettings, &ChatRequest, &dyn Fn() -> bool) -> Result<ChatResponse, ProviderError> + Send>;

/// How often a request being sent checks whether it is still wanted.
const WANTED_POLL: Duration = Duration::from_millis(100);

type Ask = (Ticket, AgentSettings, ChatRequest);
pub type Done = (Ticket, Result<ChatResponse, ProviderError>);

/// Requests waiting behind the one being sent. The agent sends one at a time; a cancelled
/// request can still be in flight when the next is asked.
const QUEUE: usize = 4;

pub struct Worker {
    asks: SyncSender<Ask>,
    done: Receiver<Done>,
    /// The one request the agent waits for; `None` when it waits for none.
    wanted: Arc<Mutex<Option<Ticket>>>,
}

/// Whether `ticket` is the request the agent waits for. A poisoned lock can't say, so the request
/// goes ahead, as it would have without this check.
fn is_wanted(wanted: &Mutex<Option<Ticket>>, ticket: Ticket) -> bool {
    wanted.lock().map_or(true, |w| *w == Some(ticket))
}

impl Worker {
    /// The thread over `send`; `wake` asks for a frame when a reply arrives.
    pub fn spawn(mut send: SendFn, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, Error> {
        let (asks, inbox) = sync_channel::<Ask>(QUEUE);
        let (outbox, done) = sync_channel::<Done>(QUEUE);
        let wanted = Arc::new(Mutex::new(None));
        let watched = wanted.clone();
        std::thread::Builder::new()
            .name("nodecraft-agent".into())
            .spawn(move || {
                for (ticket, settings, request) in inbox {
                    let still = || is_wanted(&watched, ticket);
                    if !still() {
                        continue;
                    }
                    // A keychain backend or reply that panics fails this request, not the thread.
                    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| send(&settings, &request, &still)))
                        .unwrap_or_else(|_| Err(ProviderError::new(nodecraft_providers::ErrorKind::Server, "asking the model failed unexpectedly")));
                    if outbox.send((ticket, reply)).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the agent thread: {e}")))?;
        Ok(Self { asks, done, wanted })
    }

    /// The thread the app uses: its own runtime and HTTP client, keys from the OS keychain.
    /// `loaded` is the provider settings the app started with; each request also reads the file
    /// as it is now, and both must allow a custom base URL's routing (#960).
    pub fn live(wake: Arc<dyn Fn() + Send + Sync>, loaded: ProviderSettings) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the agent thread: {e}")))?;
        let http: Arc<dyn nodecraft_providers::http::Http> =
            Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let keys = nodecraft_engine::secrets::KeyringStore;
        Self::spawn(
            Box::new(move |settings, request, still| {
                // The file as it is now, beside the settings loaded at startup: an unreadable file
                // never lowers protection below what was loaded (#960).
                let providers = [loaded.clone(), nodecraft_engine::settings::peek().providers];
                // A custom base URL gets the custom-endpoint client: no spend header, no resend.
                let client = match routed(settings, &providers, &keys)? {
                    (endpoint, Some(base)) => LlmClient::with_base_url(endpoint, &base, http.clone()),
                    (endpoint, None) => LlmClient::new(endpoint, http.clone()),
                };
                runtime.block_on(async {
                    let chat = std::pin::pin!(client.chat(request));
                    let abandoned = std::pin::pin!(async {
                        while still() {
                            tokio::time::sleep(WANTED_POLL).await;
                        }
                    });
                    match futures::future::select(chat, abandoned).await {
                        futures::future::Either::Left((reply, _)) => reply,
                        futures::future::Either::Right(_) => Err(ProviderError::new(nodecraft_providers::ErrorKind::Cancelled, "the request was cancelled")),
                    }
                })
            }),
            wake,
        )
    }

    /// Queues `request`; refused as busy when too many are waiting.
    pub fn send(&self, ticket: Ticket, settings: &AgentSettings, request: ChatRequest) -> Result<(), Error> {
        if let Ok(mut wanted) = self.wanted.lock() {
            *wanted = Some(ticket);
        }
        self.asks.try_send((ticket, settings.clone(), request)).map_err(|e| match e {
            TrySendError::Full(_) => Error::new(ErrorKind::Busy, "the model is still answering earlier requests; try again in a moment"),
            TrySendError::Disconnected(_) => Error::new(ErrorKind::Internal, "the agent thread stopped; restart NodeCraft to use the agent"),
        })
    }

    /// The agent waits for no reply: drop what is queued and abandon what is being sent.
    pub fn forget(&self) {
        if let Ok(mut wanted) = self.wanted.lock() {
            *wanted = None;
        }
    }

    /// A reply that has arrived, if any.
    pub fn poll(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }
}

/// Where the settings say to send requests, with the endpoint's key from `keys`. See
/// [`routed`]; a custom base URL is applied to the endpoint's address (tests read it).
#[cfg(test)]
pub fn endpoint(settings: &AgentSettings, providers: &[ProviderSettings], keys: &dyn SecretStore) -> Result<Endpoint, ProviderError> {
    let (mut endpoint, base) = routed(settings, providers, keys)?;
    if let Some(base) = base {
        endpoint.base_url = base.as_str().to_owned();
    }
    Ok(endpoint)
}

/// The endpoint the settings name, with its key, and the custom base URL it is served from, if
/// any. OpenAI and Anthropic follow a custom base URL set for them (#960), so their key goes to
/// that host only. Every view in `providers` (the settings loaded at startup and the file as it
/// is now) must agree: an unusable entry in any, a key set up for another provider's custom base
/// URL in any, or views that name different addresses refuse rather than reaching the
/// provider's own API.
pub fn routed(settings: &AgentSettings, providers: &[ProviderSettings], keys: &dyn SecretStore) -> Result<(Endpoint, Option<BaseUrl>), ProviderError> {
    let refused = |message: String| ProviderError::new(nodecraft_providers::ErrorKind::UnsupportedInput, message);
    // Checked before the key is read, so a refused setting never opens the keychain.
    let base = |provider: &str| -> Result<Option<BaseUrl>, ProviderError> {
        let mut agreed: Option<Option<BaseUrl>> = None;
        for view in providers {
            if let Some(conflict) = nodecraft_engine::custom_endpoints::shared_key_conflict(view, provider) {
                return Err(refused(conflict.message));
            }
            let base = nodecraft_engine::custom_endpoints::base_url(view, provider)
                .map_err(|error| refused(format!("the {provider} custom base URL in settings can't be used: {}", error.message)))?;
            match &agreed {
                Some(before) if *before != base => {
                    return Err(refused(format!("the {provider} custom base URL changed since NodeCraft started; restart it to use the new one")));
                }
                Some(_) => {}
                None => agreed = Some(base),
            }
        }
        Ok(agreed.flatten())
    };
    let key = |name: &str| -> Result<ApiKey, ProviderError> {
        let account = Account::new(name).map_err(|_| ProviderError::new(nodecraft_providers::ErrorKind::Auth, "the key's account name is invalid"))?;
        match keys.get(&account) {
            Ok(Some(secret)) => Ok(ApiKey::from(&secret)),
            Ok(None) => Err(ProviderError::new(nodecraft_providers::ErrorKind::Auth, format!("add a key for {name} in API keys to use the agent"))),
            Err(e) => Err(ProviderError::new(nodecraft_providers::ErrorKind::Auth, format!("the {name} key can't be read: {e}"))),
        }
    };
    Ok(match settings.endpoint {
        AgentEndpoint::Anthropic => {
            let base = base("anthropic")?;
            (Endpoint::anthropic(key("anthropic")?), base)
        }
        AgentEndpoint::Openai => {
            let base = base("openai")?;
            (Endpoint::openai(key("openai")?), base)
        }
        AgentEndpoint::Openrouter => (Endpoint::openrouter(key("openrouter")?), None),
        AgentEndpoint::Ollama => (Endpoint::ollama(settings.base_url()), None),
        AgentEndpoint::LmStudio => (Endpoint::lm_studio(settings.base_url()), None),
    })
}

/// The model the settings name, or the endpoint's default; `None` when the endpoint has none.
pub fn model(settings: &AgentSettings) -> Option<String> {
    match (settings.model(), settings.endpoint) {
        (Some(model), _) => Some(model.to_owned()),
        (None, AgentEndpoint::Anthropic) => Some(nodecraft_providers::llm::ANTHROPIC_DEFAULT_MODEL.to_owned()),
        (None, _) => None,
    }
}
