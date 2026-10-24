//! Background saves (#461): a save's file work (archive, durability syncs, recovery checkpoint)
//! runs off the UI thread when the host installs a [`SaveRunner`]. The engine starts no threads
//! itself; hosts without one (the browser, the CLI) keep saving inline.
//! Read-only exports (#587) reuse this executor through `Session::set_export_runner`, with
//! their own receipt/cancellation state. They retain an asset snapshot without lending storage
//! and never make document edits wait. Rejected export jobs are dropped, never run inline.

/// One file operation's work. A save owns the storage it writes through and returns it to its
/// session even on failure; an export owns only a read-only asset snapshot and completion guard.
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
    /// Starts `job` elsewhere, or hands it back (no thread available). Saves may then run inline;
    /// exports never do. The job must start promptly or be dropped: a save's next edit waits for
    /// it, while an export retains a bounded occupied slot until its job runs or is dropped.
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob>;
}
