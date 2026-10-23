//! Live progress from the server's WebSocket (#9). The server sends progress for the prompts a
//! client queued to the socket opened with that client's id, so the socket is opened before a
//! prompt is queued and drained whenever a run is polled. Progress is a bonus: a socket that
//! can't be opened (a `wss://` server, a browser, a proxy that refuses upgrades) leaves polling
//! to report queue position and completion, as before.
//!
//! [`Feeds`] opens sockets; [`native`] is the desktop and CLI one. Trackers live in memory, so a
//! run resumed after a restart (or retried after a lost reply) shows no fraction until it ends.
//! The socket is closed once no prompt is tracked, so an idle app never holds a server's
//! broadcasts unread.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use nodecraft_comfy::{self as comfy, ApiWorkflow, PromptProgress};

use crate::ProviderError;
use crate::provider::BoxFuture;

/// Most messages one poll reads, so a chatty server can't hold a poll.
const MAX_DRAIN: usize = 4096;
/// Most prompts tracked at once; the oldest is forgotten first.
const MAX_TRACKED: usize = 64;
/// How long after a failed open, or a lost socket, the next is tried.
const RETRY_MS: u64 = 30_000;
/// Longest node label a run's message shows.
const MAX_LABEL_CHARS: usize = 80;

/// An open socket, read without waiting.
pub trait Feed: Send {
    /// The text messages received since the last call, at most `max`. Binary messages (previews)
    /// are dropped. An error means the connection is gone.
    fn drain(&mut self, max: usize) -> Result<Vec<String>, ProviderError>;
}

/// Opens sockets to a server.
pub trait Feeds: Send + Sync {
    /// Opens `url` with `headers` (the auth header, a secret: never logged). Resolves quickly: a
    /// feed may still be connecting, and reads nothing until it is.
    fn open(&self, url: String, headers: Vec<(String, String)>) -> BoxFuture<'static, Result<Box<dyn Feed>, ProviderError>>;
}

#[derive(Default)]
struct State {
    feed: Option<Box<dyn Feed>>,
    opening: bool,
    retry_at: u64,
    tracked: VecDeque<Tracked>,
}

struct Tracked {
    prompt_id: String,
    progress: PromptProgress,
    /// What each node is called: its title, or its class.
    labels: BTreeMap<String, String>,
}

/// The socket and the prompts it reports on.
pub(crate) struct Live {
    feeds: Arc<dyn Feeds>,
    state: Mutex<State>,
}

/// Clears `opening` however [`Live::connect`] ends, including when its future is dropped.
struct Opening<'a>(&'a Live);

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        self.0.state().opening = false;
    }
}

impl Live {
    pub(crate) fn new(feeds: Arc<dyn Feeds>) -> Self {
        Self { feeds, state: Mutex::new(State::default()) }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Opens the socket unless it is open, being opened, or failed recently. Never fails: without
    /// a socket, runs report no fraction.
    pub(crate) async fn connect(&self, url: String, headers: Vec<(String, String)>, now_ms: u64) {
        {
            let mut state = self.state();
            if state.feed.is_some() || state.opening || now_ms < state.retry_at {
                return;
            }
            state.opening = true;
        }
        let opening = Opening(self);
        let opened = self.feeds.open(url, headers).await;
        drop(opening);
        let mut state = self.state();
        match opened {
            Ok(feed) => state.feed = Some(feed),
            Err(_) => state.retry_at = now_ms.saturating_add(RETRY_MS),
        }
    }

    /// Starts tracking `prompt_id`, which runs `workflow`.
    pub(crate) fn track(&self, prompt_id: &str, workflow: &ApiWorkflow) {
        let Ok(progress) = PromptProgress::new(prompt_id, workflow.ids()) else { return };
        let labels = workflow
            .nodes
            .iter()
            .map(|(id, node)| (id.clone(), node.title.as_deref().unwrap_or(&node.class_type).chars().take(MAX_LABEL_CHARS).collect()))
            .collect();
        let mut state = self.state();
        state.tracked.retain(|t| t.prompt_id != prompt_id);
        if state.tracked.len() >= MAX_TRACKED {
            state.tracked.pop_front();
        }
        state.tracked.push_back(Tracked { prompt_id: prompt_id.to_owned(), progress, labels });
    }

    /// Stops tracking `prompt_id`; the socket closes with the last one.
    pub(crate) fn forget(&self, prompt_id: &str) {
        let mut state = self.state();
        state.tracked.retain(|t| t.prompt_id != prompt_id);
        if state.tracked.is_empty() {
            state.feed = None;
        }
    }

    /// Reads what the server sent and returns `prompt_id`'s progress once it has started: the
    /// fraction done and what the node running now is called.
    pub(crate) fn progress(&self, prompt_id: &str, now_ms: u64) -> Option<(f32, Option<String>)> {
        let mut state = self.state();
        let messages = match state.feed.as_mut().map(|feed| feed.drain(MAX_DRAIN)) {
            Some(Ok(messages)) => messages,
            // The connection is gone (or never opened); a later submit or poll opens another.
            Some(Err(_)) => {
                state.feed = None;
                state.retry_at = now_ms.saturating_add(RETRY_MS);
                Vec::new()
            }
            None => Vec::new(),
        };
        for message in messages {
            // Unknown or malformed messages are skipped.
            let Ok(event) = comfy::parse_text(&message) else { continue };
            for tracked in state.tracked.iter_mut() {
                // A tracker over its limits stops counting; the run still finishes by polling.
                let _ = tracked.progress.apply(&event);
            }
        }
        let tracked = state.tracked.iter().find(|t| t.prompt_id == prompt_id)?;
        let progress = &tracked.progress;
        let node = progress.current_node().map(|id| tracked.labels.get(id).cloned().unwrap_or_else(|| id.chars().take(MAX_LABEL_CHARS).collect()));
        progress.started().then(|| (progress.fraction(), node))
    }

    /// True while some prompt is tracked, so a lost socket is worth opening again.
    pub(crate) fn wanted(&self) -> bool {
        !self.state().tracked.is_empty()
    }
}

/// Sockets over a plain TCP connection, opened on a short-lived thread so the run's task never
/// blocks. `wss://` isn't supported (the workspace's WebSocket client has no TLS): those servers
/// report progress by polling only.
#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use super::{Feed, Feeds};
    use crate::provider::BoxFuture;
    use crate::{ErrorKind, ProviderError};
    use std::io::ErrorKind as IoKind;
    use std::net::{TcpStream, ToSocketAddrs};
    use std::sync::mpsc::{self, Receiver, TryRecvError};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;
    use tungstenite::client::IntoClientRequest;
    use tungstenite::http::{HeaderName, HeaderValue};
    use tungstenite::protocol::WebSocketConfig;
    use tungstenite::{Message, WebSocket};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    /// Longest a submit or poll waits for the socket; a slower one is used once it is open.
    const OPEN_WAIT: Duration = Duration::from_secs(1);
    /// Previews are the largest messages a server sends; they are read and dropped.
    const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
    /// Progress messages are small; a larger text message is dropped.
    const MAX_TEXT_BYTES: usize = 1024 * 1024;
    /// Most bytes one drain reads, previews included, so a busy server can't hold a poll.
    const MAX_DRAIN_BYTES: usize = 4 * 1024 * 1024;

    #[derive(Debug, Default, Clone, Copy)]
    pub struct NativeFeeds;

    type Opened = Result<WebSocket<TcpStream>, ProviderError>;

    enum NativeFeed {
        Opening(Receiver<Opened>),
        Open(Box<WebSocket<TcpStream>>),
    }

    fn failed(why: &str) -> ProviderError {
        ProviderError::new(ErrorKind::Network, format!("the ComfyUI progress socket {why}"))
    }

    impl Feeds for NativeFeeds {
        fn open(&self, url: String, headers: Vec<(String, String)>) -> BoxFuture<'static, Result<Box<dyn Feed>, ProviderError>> {
            let (result_tx, result_rx) = mpsc::channel();
            let (ready_tx, ready_rx) = futures::channel::oneshot::channel::<()>();
            // Whichever comes first, the socket or the deadline, wakes the caller.
            let ready = Arc::new(Mutex::new(Some(ready_tx)));
            let wake = |ready: &Mutex<Option<futures::channel::oneshot::Sender<()>>>| {
                if let Some(tx) = ready.lock().unwrap_or_else(PoisonError::into_inner).take() {
                    let _ = tx.send(());
                }
            };
            let on_open = ready.clone();
            let opener = std::thread::Builder::new().name("comfy-progress".into()).spawn(move || {
                let _ = result_tx.send(connect(&url, &headers));
                wake(&on_open);
            });
            // If the timer can't start, the opener's bounded timeouts still wake the caller.
            let _ = std::thread::Builder::new().name("comfy-progress-wait".into()).spawn(move || {
                std::thread::sleep(OPEN_WAIT);
                wake(&ready);
            });
            Box::pin(async move {
                opener.map_err(|_| failed("couldn't be started"))?;
                let _ = ready_rx.await;
                let mut feed = NativeFeed::Opening(result_rx);
                feed.settle()?;
                Ok(Box::new(feed) as Box<dyn Feed>)
            })
        }
    }

    impl NativeFeed {
        /// Takes the socket if it has opened; an error if it couldn't be.
        fn settle(&mut self) -> Result<(), ProviderError> {
            if let NativeFeed::Opening(rx) = self {
                match rx.try_recv() {
                    Ok(Ok(socket)) => *self = NativeFeed::Open(Box::new(socket)),
                    Ok(Err(e)) => return Err(e),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => return Err(failed("couldn't be opened")),
                }
            }
            Ok(())
        }
    }

    fn connect(url: &str, headers: &[(String, String)]) -> Opened {
        let mut request = url.into_client_request().map_err(|_| failed("address isn't usable"))?;
        if request.uri().scheme_str() != Some("ws") {
            return Err(ProviderError::new(ErrorKind::UnsupportedInput, "progress over wss:// isn't supported; runs report progress by polling"));
        }
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| failed("header name isn't valid"))?;
            let value = HeaderValue::from_str(value).map_err(|_| failed("header value isn't valid"))?;
            request.headers_mut().insert(name, value);
        }
        let host = request.uri().host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_owned();
        let port = request.uri().port_u16().unwrap_or(80);
        let addresses = (host.as_str(), port).to_socket_addrs().map_err(|_| failed("host can't be found"))?;
        let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE_BYTES)).max_frame_size(Some(MAX_MESSAGE_BYTES));
        for address in addresses {
            let Ok(stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else { continue };
            let _ = stream.set_nodelay(true);
            // The handshake is blocking; the timeouts bound it (and are moot once nonblocking).
            if stream.set_read_timeout(Some(CONNECT_TIMEOUT)).is_err() || stream.set_write_timeout(Some(CONNECT_TIMEOUT)).is_err() {
                continue;
            }
            if let Ok((socket, _)) = tungstenite::client::client_with_config(request.clone(), stream, Some(config)) {
                socket.get_ref().set_nonblocking(true).map_err(|_| failed("couldn't be set up"))?;
                return Ok(socket);
            }
        }
        Err(failed("couldn't be opened"))
    }

    impl Feed for NativeFeed {
        fn drain(&mut self, max: usize) -> Result<Vec<String>, ProviderError> {
            self.settle()?;
            let NativeFeed::Open(socket) = self else { return Ok(Vec::new()) };
            let mut messages = Vec::new();
            let mut bytes = 0usize;
            while messages.len() < max && bytes < MAX_DRAIN_BYTES {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        bytes = bytes.saturating_add(text.len());
                        if text.len() <= MAX_TEXT_BYTES {
                            messages.push(text.as_str().to_owned());
                        }
                    }
                    Ok(Message::Binary(data)) => bytes = bytes.saturating_add(data.len()),
                    Ok(Message::Close(_)) => return Err(failed("was closed")),
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e)) if matches!(e.kind(), IoKind::WouldBlock | IoKind::TimedOut) => break,
                    Err(_) => return Err(failed("was lost")),
                }
            }
            Ok(messages)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures::executor::block_on;

    /// A socket that hands out scripted messages; `None` in the script loses the connection.
    pub(crate) struct Scripted {
        pub(crate) messages: Arc<Mutex<VecDeque<Option<String>>>>,
        pub(crate) opens: Arc<Mutex<u32>>,
        pub(crate) refuse: bool,
    }

    struct ScriptedFeed(Arc<Mutex<VecDeque<Option<String>>>>);

    impl Feed for ScriptedFeed {
        fn drain(&mut self, max: usize) -> Result<Vec<String>, ProviderError> {
            let mut queue = self.0.lock().unwrap();
            let mut out = Vec::new();
            while out.len() < max {
                match queue.pop_front() {
                    Some(Some(message)) => out.push(message),
                    Some(None) => return Err(ProviderError::new(crate::ErrorKind::Network, "lost")),
                    None => break,
                }
            }
            Ok(out)
        }
    }

    impl Feeds for Scripted {
        fn open(&self, _url: String, _headers: Vec<(String, String)>) -> BoxFuture<'static, Result<Box<dyn Feed>, ProviderError>> {
            *self.opens.lock().unwrap() += 1;
            let result: Result<Box<dyn Feed>, ProviderError> =
                if self.refuse { Err(ProviderError::new(crate::ErrorKind::Network, "refused")) } else { Ok(Box::new(ScriptedFeed(self.messages.clone()))) };
            Box::pin(async move { result })
        }
    }

    pub(crate) fn text(s: &str) -> Option<String> {
        Some(s.to_owned())
    }

    /// Opens that never finish, to drop a connect midway.
    struct Hanging(Arc<Mutex<u32>>);

    impl Feeds for Hanging {
        fn open(&self, _url: String, _headers: Vec<(String, String)>) -> BoxFuture<'static, Result<Box<dyn Feed>, ProviderError>> {
            *self.0.lock().unwrap() += 1;
            Box::pin(futures::future::pending())
        }
    }

    fn workflow(ids: &[&str]) -> ApiWorkflow {
        let nodes: serde_json::Map<String, serde_json::Value> =
            ids.iter().map(|id| ((*id).to_owned(), serde_json::json!({"class_type": "KSampler", "inputs": {}}))).collect();
        ApiWorkflow::parse(serde_json::Value::Object(nodes).to_string().as_bytes(), None).unwrap()
    }

    #[test]
    fn progress_follows_the_prompts_events() {
        let messages = Arc::new(Mutex::new(VecDeque::new()));
        let feeds = Scripted { messages: messages.clone(), opens: Arc::default(), refuse: false };
        let opens = feeds.opens.clone();
        let live = Live::new(Arc::new(feeds));
        block_on(live.connect("ws://h/ws".into(), vec![], 0));
        block_on(live.connect("ws://h/ws".into(), vec![], 0));
        assert_eq!(*opens.lock().unwrap(), 1, "an open socket is reused");
        live.track("p1", &workflow(&["1", "2"]));
        assert_eq!(live.progress("p1", 0), None, "not started yet");
        messages.lock().unwrap().extend([
            text(r#"{"type":"execution_start","data":{"prompt_id":"p1"}}"#),
            text(r#"{"type":"executing","data":{"node":"1","prompt_id":"p1"}}"#),
            text(r#"{"type":"executing","data":{"node":"2","prompt_id":"p1"}}"#),
            text(r#"{"type":"progress","data":{"value":5,"max":10,"node":"2","prompt_id":"p1"}}"#),
            text("not json"),
            text(r#"{"type":"executing","data":{"node":"9","prompt_id":"other"}}"#),
        ]);
        let (fraction, node) = live.progress("p1", 0).unwrap();
        assert!((fraction - 0.75).abs() < 1e-6, "{fraction}");
        assert_eq!(node.as_deref(), Some("KSampler"));
        assert_eq!(live.progress("unknown", 0), None);
        live.forget("p1");
        assert!(!live.wanted());
        assert!(live.state().feed.is_none(), "the socket closes with the last prompt");
        block_on(live.connect("ws://h/ws".into(), vec![], 0));
        assert_eq!(*opens.lock().unwrap(), 2, "and opens again for the next");
    }

    #[test]
    fn a_lost_socket_is_reopened_after_a_pause() {
        let messages = Arc::new(Mutex::new(VecDeque::from([None])));
        let feeds = Scripted { messages: messages.clone(), opens: Arc::default(), refuse: false };
        let opens = feeds.opens.clone();
        let live = Live::new(Arc::new(feeds));
        block_on(live.connect("ws://h/ws".into(), vec![], 0));
        live.track("p1", &workflow(&["1"]));
        assert_eq!(live.progress("p1", 5), None);
        assert!(live.state().feed.is_none(), "a lost socket is dropped");
        block_on(live.connect("ws://h/ws".into(), vec![], 6));
        assert_eq!(*opens.lock().unwrap(), 1, "not reopened at once");
        block_on(live.connect("ws://h/ws".into(), vec![], 5 + RETRY_MS));
        assert_eq!(*opens.lock().unwrap(), 2);
        messages.lock().unwrap().extend([text(r#"{"type":"execution_start","data":{"prompt_id":"p1"}}"#)]);
        assert_eq!(live.progress("p1", 5 + RETRY_MS).map(|(f, _)| f), Some(0.0));
    }

    #[test]
    fn a_connect_dropped_midway_lets_the_next_one_open() {
        let opens = Arc::new(Mutex::new(0));
        let live = Live::new(Arc::new(Hanging(opens.clone())));
        {
            let mut connecting = Box::pin(live.connect("ws://h/ws".into(), vec![], 0));
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            assert!(connecting.as_mut().poll(&mut cx).is_pending());
        }
        let mut connecting = Box::pin(live.connect("ws://h/ws".into(), vec![], 0));
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(connecting.as_mut().poll(&mut cx).is_pending());
        assert_eq!(*opens.lock().unwrap(), 2, "the dropped open didn't block the next");
    }

    #[test]
    fn a_refused_socket_is_retried_later_and_tracking_is_bounded() {
        let feeds = Scripted { messages: Arc::default(), opens: Arc::default(), refuse: true };
        let opens = feeds.opens.clone();
        let live = Live::new(Arc::new(feeds));
        block_on(live.connect("ws://h/ws".into(), vec![], 1_000));
        block_on(live.connect("ws://h/ws".into(), vec![], 1_000 + RETRY_MS - 1));
        assert_eq!(*opens.lock().unwrap(), 1, "not retried at once");
        block_on(live.connect("ws://h/ws".into(), vec![], 1_000 + RETRY_MS));
        assert_eq!(*opens.lock().unwrap(), 2);
        for i in 0..MAX_TRACKED + 5 {
            live.track(&format!("p{i}"), &workflow(&["1"]));
        }
        assert_eq!(live.state().tracked.len(), MAX_TRACKED);
        assert!(live.state().tracked.iter().all(|t| t.prompt_id != "p0"), "the oldest is forgotten");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_native_socket_reads_a_real_server() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/ws?clientId=c", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket.send(tungstenite::Message::binary(vec![0u8; 4096])).unwrap();
            socket.send(tungstenite::Message::text(r#"{"type":"status","data":{}}"#)).unwrap();
            socket.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
        let mut feed = block_on(native::NativeFeeds.open(url, vec![("x-api-key".into(), "k".into())])).unwrap();
        let mut got = Vec::new();
        for _ in 0..100 {
            got.extend(feed.drain(10).unwrap());
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got, vec![r#"{"type":"status","data":{}}"#.to_owned()], "previews are dropped");
        server.join().unwrap();
        let tls = block_on(native::NativeFeeds.open("wss://127.0.0.1:1/ws".into(), vec![]));
        assert!(tls.is_err());
    }
}
