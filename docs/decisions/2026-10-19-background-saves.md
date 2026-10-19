## 2026-10-19 · ⌘S writes the file on a save thread; later changes wait for it (#461)

- **Why.** A native save spends ~28 ms of its ~32 ms in six full syncs (archive, journal,
  destination and its directory, checkpoint and its directory), which held the UI frame for
  ~53 ms. Serialising the snapshot takes ~1.5 ms, so that part stays on the calling thread.
- **The host lends its storage.** The session builds the snapshot, then hands its
  `ProjectStorage + Send` to a host `SaveRunner` as one `SaveJob`, which writes, syncs and
  checkpoints and sends the storage back, also when the work fails or panics. The engine starts
  no threads; the native app's runner spawns a `nodecraft-save` thread. A host without a runner,
  a storage that isn't `Send`, or a runner that can't start a thread saves inline.
- **Opt-in per call.** `file.save` and `file.save_as` take `background: bool` (default `false`).
  Only the app's ⌘S passes it and gets `{saved:false, saving:true}`; it polls `Session::poll_save`
  each frame, reports a failure as a notice, and `FileStatus.saving` adds "(saving…)" to the
  title. CLI, control and engine callers keep the synchronous `{saved:true}` result.
- **Nothing is refused while saving; changes wait.** Any command that edits the document, assets
  or storage (and imports, recovery, closing) first waits for the pending save, so the file holds
  exactly the snapshotted state and storage calls happen in the order an inline save would make
  them. A stalled disk therefore holds up the next edit rather than ⌘S itself. Read-only commands (no-change, perf, service and prepared-run effects) don't wait. The
  outcome a change waited for is kept for the host's next poll.
- **A lost job is not silent.** If a job is dropped unrun, the session reports an internal error,
  marks recovery required and refuses further storage work rather than journal edits nowhere.
- **Rejected:** running the job on the async exec runtime (blocking syncs would stall its
  workers), and copying the storage (locks and the recovery journal belong to one owner).
