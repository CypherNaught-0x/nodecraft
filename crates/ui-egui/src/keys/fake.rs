//! A keychain whose every call waits for the test to answer it over a channel, so a test decides
//! when (and whether) a save, removal or read finishes, independently of the UI's frames and
//! timeouts. Keys are kept in a [`MemoryStore`]; nothing touches a real keychain.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nodecraft_engine::secrets::{Account, MemoryStore, Secret, SecretError, SecretStore};

/// Which call the keychain is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    Get,
    Set,
    Delete,
}

/// What the test sends: let the call go ahead, or fail it.
pub(crate) type Reply = Result<(), SecretError>;

struct ChannelStore {
    memory: Arc<MemoryStore>,
    entered: Sender<Call>,
    replies: Mutex<Receiver<Reply>>,
    exited: Sender<()>,
}

impl ChannelStore {
    fn wait(&self, call: Call) -> Result<(), SecretError> {
        let closed = || SecretError::Failed("fixture closed".into());
        self.entered.send(call).map_err(|_| closed())?;
        self.replies.lock().map_err(|_| closed())?.recv().map_err(|_| closed())?
    }
}

impl SecretStore for ChannelStore {
    fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
        self.wait(Call::Get)?;
        self.memory.get(account)
    }
    fn set(&self, account: &Account, secret: &Secret) -> Result<(), SecretError> {
        self.wait(Call::Set)?;
        self.memory.set(account, secret)
    }
    fn delete(&self, account: &Account) -> Result<bool, SecretError> {
        self.wait(Call::Delete)?;
        self.memory.delete(account)
    }
}

impl Drop for ChannelStore {
    fn drop(&mut self) {
        let _ = self.exited.send(());
    }
}

/// The test's side of the channel keychain.
pub(crate) struct Keychain {
    pub memory: Arc<MemoryStore>,
    entered: Receiver<Call>,
    replies: Sender<Reply>,
    exited: Receiver<()>,
}

const WAIT: Duration = Duration::from_secs(10);

impl Keychain {
    /// A store whose calls wait on the returned [`Keychain`].
    pub(crate) fn new() -> (Arc<dyn SecretStore>, Self) {
        let memory = Arc::new(MemoryStore::default());
        let (entered, entering) = channel();
        let (replies, replying) = channel();
        let (exited, exiting) = channel();
        let store = ChannelStore { memory: Arc::clone(&memory), entered, replies: Mutex::new(replying), exited };
        (Arc::new(store), Self { memory, entered: entering, replies, exited: exiting })
    }

    /// Waits until the keychain is asked `call`.
    pub(crate) fn entered(&self, call: Call) {
        assert_eq!(self.entered.recv_timeout(WAIT).expect("the keychain was never called"), call);
    }

    /// Whether the keychain has been asked anything not yet waited for.
    pub(crate) fn idle(&self) -> bool {
        self.entered.try_recv().is_err()
    }

    /// Answers the call the keychain is waiting on.
    pub(crate) fn answer(&self, reply: Reply) {
        self.replies.send(reply).expect("the keychain has stopped");
    }

    /// Waits for the worker to drop the store (after its results half is gone).
    pub(crate) fn closed(&self) {
        self.exited.recv_timeout(WAIT).expect("the keychain worker never stopped");
    }
}
