//! The agent's requests to its chat endpoint, sent on a thread of their own so a slow model or a
//! keychain prompt never holds up a frame. The thread reads the endpoint's key when it sends, and
//! hands each reply back with the ticket it answers; the agent ignores replies it no longer wants.
use nodecraft_agent::Ticket;
use nodecraft_engine::secrets::{Account, SecretStore};
use nodecraft_engine::settings::{AgentEndpoint, AgentSettings};
use nodecraft_providers::ProviderError;
use nodecraft_providers::llm::{ApiKey, ChatRequest, ChatResponse, Endpoint, LlmClient};
use nodecraft_types::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// Sends one request with the given settings; the app's reads the keychain and the network, tests
/// pass a fixture.
pub type SendFn = Box<dyn FnMut(&AgentSettings, &ChatRequest) -> Result<ChatResponse, ProviderError> + Send>;

type Ask = (Ticket, AgentSettings, ChatRequest);
pub type Done = (Ticket, Result<ChatResponse, ProviderError>);

/// Requests waiting behind the one being sent. The agent sends one at a time; a cancelled
/// request can still be in flight when the next is asked.
const QUEUE: usize = 4;

pub struct Worker {
    asks: SyncSender<Ask>,
    done: Receiver<Done>,
}

impl Worker {
    /// The thread over `send`; `wake` asks for a frame when a reply arrives.
    pub fn spawn(mut send: SendFn, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, Error> {
        let (asks, inbox) = sync_channel::<Ask>(QUEUE);
        let (outbox, done) = sync_channel::<Done>(QUEUE);
        std::thread::Builder::new()
            .name("nodecraft-agent".into())
            .spawn(move || {
                for (ticket, settings, request) in inbox {
                    // A keychain backend or reply that panics fails this request, not the thread.
                    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| send(&settings, &request)))
                        .unwrap_or_else(|_| Err(ProviderError::new(nodecraft_providers::ErrorKind::Server, "asking the model failed unexpectedly")));
                    if outbox.send((ticket, reply)).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the agent thread: {e}")))?;
        Ok(Self { asks, done })
    }

    /// The thread the app uses: its own runtime and HTTP client, keys from the OS keychain.
    pub fn live(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the agent thread: {e}")))?;
        let http: Arc<dyn nodecraft_providers::http::Http> =
            Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let keys = nodecraft_engine::secrets::KeyringStore;
        Self::spawn(
            Box::new(move |settings, request| {
                let client = LlmClient::new(endpoint(settings, &keys)?, http.clone());
                runtime.block_on(client.chat(request))
            }),
            wake,
        )
    }

    /// Queues `request`; refused as busy when too many are waiting.
    pub fn send(&self, ticket: Ticket, settings: &AgentSettings, request: ChatRequest) -> Result<(), Error> {
        self.asks.try_send((ticket, settings.clone(), request)).map_err(|e| match e {
            TrySendError::Full(_) => Error::new(ErrorKind::Busy, "the model is still answering earlier requests; try again in a moment"),
            TrySendError::Disconnected(_) => Error::new(ErrorKind::Internal, "the agent thread stopped; restart NodeCraft to use the agent"),
        })
    }

    /// A reply that has arrived, if any.
    pub fn poll(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }
}

/// Where the settings say to send requests, with the endpoint's key from `keys`.
pub fn endpoint(settings: &AgentSettings, keys: &dyn SecretStore) -> Result<Endpoint, ProviderError> {
    let key = |name: &str| -> Result<ApiKey, ProviderError> {
        let account = Account::new(name).map_err(|_| ProviderError::new(nodecraft_providers::ErrorKind::Auth, "the key's account name is invalid"))?;
        match keys.get(&account) {
            Ok(Some(secret)) => Ok(ApiKey::from(&secret)),
            Ok(None) => Err(ProviderError::new(nodecraft_providers::ErrorKind::Auth, format!("add a key for {name} in API keys to use the agent"))),
            Err(e) => Err(ProviderError::new(nodecraft_providers::ErrorKind::Auth, format!("the {name} key can't be read: {e}"))),
        }
    };
    Ok(match settings.endpoint {
        AgentEndpoint::Anthropic => Endpoint::anthropic(key("anthropic")?),
        AgentEndpoint::Openai => Endpoint::openai(key("openai")?),
        AgentEndpoint::Openrouter => Endpoint::openrouter(key("openrouter")?),
        AgentEndpoint::Ollama => Endpoint::ollama(settings.base_url()),
        AgentEndpoint::LmStudio => Endpoint::lm_studio(settings.base_url()),
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
