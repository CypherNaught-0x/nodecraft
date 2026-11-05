## 2026-11-04 · Recover runs in the background, nothing on the UI thread waits for a save, and what is left of #367 (#367)

Follows [the startup and last-save opens](2026-11-04-background-startup-and-last-save-opens.md)
and [the inventory and lock order](2026-11-04-persistence-inventory-and-lock-order.md), which left
Recover inline, the Windows checkpoint replace out of the checks made inside an open, and
changes from the control channel and MCP waiting for a save on the UI thread.

- **Recover is a third background operation.** `Persistence::recover` runs as a `SaveJob` with
  the lent storage, like a save or an open, under `FileOperationKind::Recover`. On the job's
  thread it migrates a legacy identity to the file's scope (keeping the pooled identity when the
  scope can't be read, as before), copies the recovered media into this session's workspace
  (`AssetStore::in_context`), seals the archive and publishes the checkpoint. Phases are
  `preparing` (the copies and the archive) and `checkpointing`. `Session::recover_in_background`
  starts it and returns `{recovered:false,recovering:true}`; `Session::poll_file` gives
  `{recovered:true,read_only:false}` or the error. `Session::recover` keeps its inline contract
  through the same code path (no runner), so web recovery, tests and other hosts are unchanged.
  - *One owner, refused while it runs.* Until it settles, every change, run, import, file command
    and another recovery is refused as `Busy` naming `file.cancel`, as during an open: the
    document is about to be replaced. Reads still answer.
  - *Truthful cancel.* `file.cancel` stops it until the checkpoint publishes. Stopped or failing
    before that, nothing is adopted: the current document, its lineage and the journal stay byte
    for byte (tested on memory storage and native storage), and the outcome is "recovering was
    cancelled; the current project is unchanged" or the failure's own error. A cancel after the
    checkpoint published gets the real outcome (recovered). A job the runner drops unrun reports
    an internal error and latches recovery, as a dropped save or open does.
  - *The kept copy is never lost.* The engine never touches the copy the work came from. In the
    app, "Recover" starts the background recovery and keeps the offer in the file dialogs'
    `Recovering` step. Success shows the recovered project and keeps the copy until the work is
    saved, as before. A failure or a cancel keeps the copy and the current project, says so, and
    asks the question again, so either choice is one click away. When the host replaces the
    session meanwhile, the copy stays for the next launch and the question goes with the session
    it was asked over; the old session's job, run late, changes nothing shown. Quitting during a
    recovery cancels it, like an open. The progress window says "Recovering unsaved work…" with
    "Copying the recovered work's media…" and then "Writing the recovery copy…", Cancel enabled
    throughout; the title says "(recovering…)". While it runs the question counts as undecided,
    so queued runs stay paused.
  - *Space.* A recovery writes only to the recovery drive: `SpaceInventory::recover` charges the
    media copies (every byte the recovered store holds outside this workspace) and the checkpoint,
    plus the Windows journal replace and the cleanup-debt note. `ProjectStorage::recovery_free`
    asks about that drive whatever file the work belongs to, so untitled work is checked too. The
    refusal (`Busy`, sizes and never paths) comes before the job starts. The slice-1 record said
    Recover writes about one archive; it writes the copies too, about two.
  - *`share.adopt` stays inline.* Adopting a joiner's shared project writes a checkpoint of media
    already in this workspace (nothing copied) on the calling thread, as before.
- **The startup keep-aside stays on the main thread, and is what is left of #367.** After a
  crash, three steps run before the window exists, with no progress and no cancel:
  `keep_unrecovered` replays the live journal in full to decide whether it holds work
  (`holds_work`), copies the whole journal to a `session.journal.unrecovered-<n>` slot and syncs
  it (`std::fs::copy` and `sync_all`: about one journal's bytes, which no space inventory charges),
  and then `RecoveryOffer::read` replays that copy in full to say what the question offers. Also,
  while an unsaved, failed or cancelled copy is kept, `RecoveryOffer::at_startup` replays kept
  copies on every launch, trying up to `MAX_KEPT` (99) slots until one replays. All of it streams
  (memory stays bounded) but is bounded only by the journals' own limits, which for a streaming
  journal are the project's size; nothing else (control, MCP, the agent) has started meanwhile.
  The keep-aside must finish before the session is built over the journal, whose first
  checkpoint replaces it (#408), so moving it means starting the session without that
  checkpoint, or behind a window that can't yet own the journal. That is a change to recovery's
  ownership rules, not a reuse of the job machinery, and it is left to a follow-up issue rather
  than half done here. The commit closing this slice therefore says `Part of #367`.
- **Nothing on the UI thread waits for a save.** A change made during a background save used to
  wait for it (`settle_for_edit`), on whatever thread made it. In the desktop that is the UI
  thread, so an agent's edit during a long save held the window for as long as the disk took. Now:
  - *Surfaces save and open in the background.* A `file.save`, `file.save_as` or `file.open`
    from the control channel or MCP runs on the save runner whatever its `background` says
    (`LiveCommands`), so a plain save from a client no longer writes on the UI thread; while any
    save or open runs, another one is refused as `Busy` naming `file.cancel` (also in `enabled`,
    after settling a save that has finished). The in-canvas agent has no allowed folders and is
    refused file commands entirely. A new read-only `file.status` reports the operation in
    flight and how the last one ended (`last`, with the error when it failed), plus `dirty`,
    `titled`, `read_only` and `recovery_required`; it names no path, since the person's own file
    isn't a client's to learn.
  - *Surfaces are refused.* `Session::dispatch_without_waiting` is dispatch, except that where it
    would wait for a save still writing it refuses as `Busy` ("the project is still being saved;
    try again when the save finishes, or cancel it with file.cancel"); a save that has already
    finished is settled first and the change lands. The desktop's control channel, MCP bridge and
    in-canvas agent all dispatch through `LiveCommands`, which uses it; Format selection, the one
    host command that edits, calls `Session::settle_without_waiting` first. Reads answer as
    before. Tested with the save's job held, so waiting would never return.
  - *Host work defers instead.* The run host's pump doesn't record finished work while a
    background save runs: results and their media stay in the run's mailbox, neither refused nor
    held, and are recorded on the first pump after the save settles (so the file holds exactly
    the state it snapshotted). What touches no document still goes on: a submitted job's handle
    is journalled (a crash meanwhile follows the job instead of sending it again under its staged
    key, which a provider ignoring the key would charge twice) and progress reports reach the
    queue panel. Live recompute leaves a finished round in its channel, live sharing leaves the
    worker's reports queued, and the agent leaves the model's replies in its worker while any
    save, open or recovery runs (each change a reply asks for would be refused as busy and
    retried as a paid turn), with Approve disabled meanwhile; each is taken in once the operation
    has settled. The person's own input is already owned by the progress window.
  - *Quitting during a save* waits for it as before, then runs a record-only pump
    (`RunHost::pump_for_quit`: it starts no accepted run and, with the host closing, sends
    nothing downstream; bounded while results' media are still being adopted) before deciding whether the recovery journal can be
    closed, so results that waited for the save are in the journal rather than lost with the run
    host.
  - *Kept:* the engine's `dispatch` still waits for direct callers (CLI and tests, which have no
    runner, and hosts that want it), and quitting still waits for a save, deliberately: until it
    publishes, the work isn't saved. This supersedes "Nothing is refused while saving; changes
    wait" in [the background saves record](2026-10-19-background-saves.md) for the desktop's
    surfaces only.
- **One operation at a time is the bound on queued operations.** A save, open or recovery owns the
  storage outright while it runs (`Store::Lent`), so a second one can't start until the first
  hands it back. It is refused as `Busy` ("a save is already in progress", "a project is still
  opening", "recovered work is still being restored"), and every dispatched refusal names
  `file.cancel`. A queue was rejected: queuing a save behind an open would save a document the
  open is about to replace, queuing an open behind a save would discard the save's outcome, and a
  queued operation would need its own cancel and identity checks against a document that changes
  while it waits. Queue depth one, refused rather than blocked, is the conservative choice.
- **The checks inside an open charge the Windows checkpoint replace.** `space::check_open_copies`
  and `space::check_rewrite` take the journal's size on disk (`ProjectStorage::journal_bytes`) and
  add `with_journal_replaced`, as the save and open preflights do. Elsewhere it adds nothing, so
  macOS and Linux numbers are unchanged; a format test checks the Windows charge.
- **The journal tail is bounded, not charged.** After a checkpoint, edits append frames; a
  streaming journal's tail is refused past `MAX_STREAM_TAIL_BYTES` (65 GiB) and the whole journal
  past `MAX_STREAM_JOURNAL_BYTES` (132 GiB), each with "save before editing". Appends are not file
  operations and aren't preflighted: an append that runs out of room fails where it happens. A
  partial append latches `recovery_required` before the host call, so a torn frame is never
  journalled over: replay stops before it and reports the torn tail, every later edit is refused
  until a save writes a new checkpoint, and that save repairs the journal
  (`background_recover367::a_partial_append_latches_until_a_checkpoint_repairs_the_journal`).
  A file operation's inventory doesn't credit the old journal (tail included) it replaces, which
  over-counts by the tail; it never under-counts it.
- **Staged imports, argued.** An import staged before a save is already on disk, so it is already
  missing from the free space the probe reports; it isn't written again by the save (the archive
  holds adopted assets only), so charging it would refuse saves that fit. An import staged while a
  background save runs competes for the asset context's operation gate: staging takes it with
  `try_lock` and is refused `Busy` while the job holds it, and between the job's gated steps it is
  ordinary concurrent disk use, which the advisory preflight already says it can't predict; its own
  quota reservation refuses it if the session's budget runs out. Not measured separately.
- **Native cancel, tested where it can be timed.** A native recovery cancelled before its job runs
  leaves the live journal file byte for byte
  (`a_cancelled_native_recovery_leaves_the_live_journal_byte_for_byte`). Inside the media copy
  the cancel lands at the next chunk of the shared copy primitive, tested below; a cancel timed
  in the middle of a native recovery's copy isn't, since no hook can stall that copy.
- **Cancel granularity, tested.** The streaming copy every body pass goes through
  (`import_stream`: a recovery's media copies, an import, a classic open's verified copies) reads
  in bounded chunks and checks the cancel between them. A test stalls a read, cancels, and shows
  the read isn't interrupted (the operation is still on it), that once it returns no further read
  is made, and that nothing half-copied is adopted. No promise is made that a blocking OS read or
  write is interrupted.

### #367's acceptance, and the evidence

| Acceptance | Evidence |
|---|---|
| Native off-UI worker, bounded queued operations, exact identity, one owner, truthful progress and cancel; AssetContext `Busy` and lock order kept; no UI or runner lock held over disk work or callbacks | **Not yet for the startup keep-aside** (above). Saves #461 (`background_save`), opens #475 (`background_open`), startup and last-save opens #1045, recover here (`background_recover367`, `recovery::tests::recover_restores_in_the_background_behind_its_progress_window`). Queue depth one, refused, above. Identity: `PendingOpen` authority, lineage checks in the app (`a_session_replaced_while_recovering_keeps_the_copy_and_drops_the_question`). Lock order: [the inventory record](2026-11-04-persistence-inventory-and-lock-order.md); no wait on the UI thread: `pump_during_save367` (a save parked inside a storage write while the run pump keeps returning), `control::tests::race367::a_control_client_meets_a_background_save_busy_instead_of_blocking_the_window`, `control::tests::files423` (plain saves and opens from a client run in the background), `agent_panel::tests::a_reply_waits_for_a_background_save_instead_of_spending_turns_on_busy`, `live::tests::a_live_result_waits_for_a_background_save_and_frames_go_on`, `sharing::tests::a_peers_edit_waits_for_a_background_save_and_ticks_go_on`, `runs_tests::quitting_during_a_save_records_the_results_that_waited_for_it` |
| Inventory every simultaneous extent and body pass; phase bounds; actionable errors without paths; preflight advisory; real write errors handled | **Not yet for the startup keep-aside copy** (above). `space.rs` and its tests, `space_preflight367`, `space_wiring367`, `persistence_faults367::the_inventory_covers_measured_saves` (#1004, #1047); the recovery inventory and its wiring test (`a_recovery_that_the_recovery_drive_cannot_hold_is_refused_before_it_writes`), the in-open Windows charge (`space367`), tail and staged imports above |
| Cancel before publication leaves old authority unchanged; after publication can't report rollback; checkpoint failure after a save truthful; latch and debt owned; partial append requires a checkpoint; bounded chunks, no promise for a blocking read | `background_open` cancel tests, `persistence_faults367` (publish then checkpoint failure, late cancel, cancelled save leaves no debt), `background_recover367` (cancel and failure before the checkpoint, late cancel, dropped job, partial append, chunk boundary) |
| Self-contained recovery, sealed-source identity, source deletion safety, history roots and pins, quota and debt accounting; no pathname checkpoint or reflinks | `persistence_faults367::work_on_a_background_opened_project_recovers_after_its_file_is_deleted`, `background_recover367::native_work_recovered_in_the_background_holds_its_media_after_the_kept_copy_goes`; checkpoints stay full archives, copies are real copies |
| Real native Session and storage paths with slow, error and cancel boundaries, concurrent and mismatched operations, publish then recovery failure, cleanup debt, recovery after source removal; UI and event pump progress independent of provider traffic; memory streaming and wasm unsupported behaviour kept | the tests above, `automation/tests/file_race367` (MCP racing an open), `pump_during_save367` (mock provider polled to the end on its own threads while the save is parked); web recovery still runs inline (`web_journal`), unchanged |
| Old compatibility and version semantics, physical evidence; modest real calibration plus synthetic controls; heavy multi-GiB proof only with explicit resource authorization | Formats untouched here; calibration in [the inventory record](2026-11-04-persistence-inventory-and-lock-order.md) (6 MB classic, 54.5 MB streaming). No multi-GiB proof was run: none was authorized |

What remains of #367: the startup keep-aside above (off the main thread with progress and a
"decide later" cancel, reusing the first replay for the offer, its copy charged in the space
inventory, and the repeated replays of kept copies bounded), for a follow-up issue. Outside #367:
a byte-accurate progress bar (phases only, as decided in #475), crash-orphan workspace cleanup
(#363) and format coordination (#359), which #367 reuses and does not replace.

- **Rejected:** running the recovery's media copies on the calling thread and only the checkpoint
  on the runner (the copies are the larger half of the work); removing the kept copy when the
  recovery publishes (it is released only by a save of the recovered work, as before); and making
  `dispatch` refuse during every save (the CLI and engine callers rely on the wait, and run
  results would have been held rather than recorded).
