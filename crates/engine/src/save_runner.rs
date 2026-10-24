//! Background saves (#461): a save's file work (archive, durability syncs, recovery checkpoint)
//! runs off the UI thread when the host installs a [`SaveRunner`]. The engine starts no threads
//! itself; hosts without one (the browser, the CLI) keep saving inline.

/// One save's file work, owning the storage it writes through. Running it always hands the
/// storage back to its session, also when the work fails or panics.
pub struct SaveJob(Box<dyn FnOnce() + Send>);

impl SaveJob {
    pub(crate) fn new(work: impl FnOnce() + Send + 'static) -> Self {
        Self(Box::new(work))
    }
    /// Runs the job on the calling thread.
    pub fn run(self) {
        (self.0)()
    }
}

/// Runs [`SaveJob`]s off the calling thread, implemented by the native host.
pub trait SaveRunner: Send + Sync {
    /// Starts `job` elsewhere, or hands it back (no thread available) to run inline. The job
    /// must start promptly or be dropped: the next edit waits for it on the calling thread.
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob>;
}
